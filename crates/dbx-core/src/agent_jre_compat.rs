//! JDK 17 compatibility for agent JARs.
//!
//! DBX agents are distributed as Java 21 bytecode (class file version 65.0),
//! which cannot load on a JDK 17 runtime. StarRocks/ArgoDB-class JDBC drivers
//! (Hive protocol) call `Thread.stop()`, which JDK 20+ removed, so the plugin
//! JRE must stay at 17. This module rewrites the class file version of any
//! class compiled at a newer major version down to 61.0 (JDK 17) so that
//! distributed agent JARs keep working on the bundled JDK 17 runtime.

use std::io::{Read, Write};
use std::path::Path;

use zip::ZipArchive;

/// Maximum class file major version supported by the bundled JDK 17 runtime.
const MAX_CLASS_MAJOR: u16 = 61;

/// Rewrites every class file compiled at a major version newer than JDK 17
/// (61.0) inside `jar_path` so it loads on the bundled JDK 17 runtime.
///
/// Only the version header bytes are touched; bytecode itself is preserved.
/// Returns the number of rewritten classes. JARs with no newer classes are
/// left untouched (the file is not rewritten when nothing changed).
pub fn downgrade_jar_to_jdk17(jar_path: &Path) -> Result<usize, String> {
    let file = std::fs::File::open(jar_path).map_err(|err| format!("Failed to open {}: {err}", jar_path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("Failed to read {}: {err}", jar_path.display()))?;

    let mut changed = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("Failed to read entry {index} of {}: {err}", jar_path.display()))?;
        let name = entry.name().to_string();
        if !name.ends_with(".class") {
            continue;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|err| format!("Failed to read {} in {}: {err}", name, jar_path.display()))?;
        if let Some(major) = class_file_major(&buf) {
            if major > MAX_CLASS_MAJOR {
                buf[6] = (MAX_CLASS_MAJOR >> 8) as u8;
                buf[7] = (MAX_CLASS_MAJOR & 0xff) as u8;
                changed += 1;
            }
        }
    }
    drop(archive);

    if changed == 0 {
        return Ok(0);
    }

    let tmp_path = jar_path.with_extension(format!("jar.jre17-{}", std::process::id()));
    let tmp_file =
        std::fs::File::create(&tmp_path).map_err(|err| format!("Failed to create {}: {err}", tmp_path.display()))?;
    let mut writer = zip::ZipWriter::new(tmp_file);

    let file = std::fs::File::open(jar_path).map_err(|err| format!("Failed to open {}: {err}", jar_path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("Failed to read {}: {err}", jar_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("Failed to read entry {index} of {}: {err}", jar_path.display()))?;
        let name = entry.name().to_string();
        let options = entry.options();
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf).map_err(|err| format!("Failed to read {name} in {}: {err}", jar_path.display()))?;
        if name.ends_with(".class") {
            if let Some(major) = class_file_major(&buf) {
                if major > MAX_CLASS_MAJOR {
                    buf[6] = (MAX_CLASS_MAJOR >> 8) as u8;
                    buf[7] = (MAX_CLASS_MAJOR & 0xff) as u8;
                }
            }
        }
        writer
            .start_file(&name, options)
            .map_err(|err| format!("Failed to write {name} to {}: {err}", tmp_path.display()))?;
        writer.write_all(&buf).map_err(|err| format!("Failed to write {name} to {}: {err}", tmp_path.display()))?;
    }
    let mut tmp_file = writer.finish().map_err(|err| format!("Failed to finalize {}: {err}", tmp_path.display()))?;
    tmp_file.flush().map_err(|err| format!("Failed to flush {}: {err}", tmp_path.display()))?;
    drop(tmp_file);

    std::fs::rename(&tmp_path, jar_path).map_err(|err| format!("Failed to replace {}: {err}", jar_path.display()))?;
    Ok(changed)
}

/// Returns the class file major version, or `None` if `buf` is not a class file.
fn class_file_major(buf: &[u8]) -> Option<u16> {
    if buf.len() < 8 || &buf[0..4] != b"\xca\xfe\xba\xbe" {
        return None;
    }
    Some(u16::from_be_bytes([buf[6], buf[7]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn class_bytes(major: u16) -> Vec<u8> {
        let mut buf = vec![0xca, 0xfe, 0xba, 0xbe, 0x00, 0x00];
        buf.extend_from_slice(&major.to_be_bytes());
        buf.extend_from_slice(&[0x00; 16]);
        buf
    }

    #[test]
    fn downgrades_newer_classes_in_jar() {
        let dir = std::env::temp_dir().join(format!("dbx-jre17-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("agent.jar");
        {
            let file = std::fs::File::create(&jar).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer.start_file("com/dbx/agent/New.class", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&class_bytes(65)).unwrap();
            writer.start_file("com/dbx/agent/Old.class", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&class_bytes(52)).unwrap();
            writer.start_file("META-INF/MANIFEST.MF", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(b"Manifest-Version: 1.0\n").unwrap();
            writer.finish().unwrap();
        }

        let changed = downgrade_jar_to_jdk17(&jar).unwrap();
        assert_eq!(changed, 1);

        let file = std::fs::File::open(&jar).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut buf = Vec::new();
        archive.by_name("com/dbx/agent/New.class").unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(class_file_major(&buf), Some(61));
        let mut buf = Vec::new();
        archive.by_name("com/dbx/agent/Old.class").unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(class_file_major(&buf), Some(52));
    }

    #[test]
    fn leaves_unchanged_jar_alone() {
        let dir = std::env::temp_dir().join(format!("dbx-jre17-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let jar = dir.join("agent.jar");
        {
            let file = std::fs::File::create(&jar).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer.start_file("Old.class", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(&class_bytes(52)).unwrap();
            writer.finish().unwrap();
        }
        let before = std::fs::metadata(&jar).unwrap().len();
        let changed = downgrade_jar_to_jdk17(&jar).unwrap();
        assert_eq!(changed, 0);
        assert_eq!(std::fs::metadata(&jar).unwrap().len(), before);
    }
}
