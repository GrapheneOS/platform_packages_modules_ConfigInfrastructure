/*
 * Copyright (C) 2024 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use crate::AconfigdError;
use openssl::hash::{Hasher, MessageDigest};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Set file permission
pub(crate) fn set_file_permission(file: &Path, mode: u32) -> Result<(), AconfigdError> {
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(file, perms).map_err(|errmsg| {
        AconfigdError::FailToUpdateFilePerm { file: file.display().to_string(), mode, errmsg }
    })?;
    Ok(())
}

/// Copy file
pub(crate) fn copy_file(src: &Path, dst: &Path, mode: u32) -> Result<(), AconfigdError> {
    if dst.exists() {
        set_file_permission(dst, 0o644)?;
    }

    let mut src_file = File::open(src).map_err(|errmsg| AconfigdError::FailToCopyFile {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        errmsg,
    })?;

    let mut dst_file = File::create(dst).map_err(|errmsg| AconfigdError::FailToCopyFile {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        errmsg,
    })?;

    std::io::copy(&mut src_file, &mut dst_file).map_err(|errmsg| {
        AconfigdError::FailToCopyFile {
            src: src.display().to_string(),
            dst: dst.display().to_string(),
            errmsg,
        }
    })?;

    // force kernel to flush file data in kernel buffer to filesystem
    dst_file.sync_all().map_err(|errmsg| AconfigdError::FailToCopyFile {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        errmsg,
    })?;

    set_file_permission(dst, mode)
}

/// Copy file without fsync
pub(crate) fn copy_file_without_fsync(
    src: &Path,
    dst: &Path,
    mode: u32,
) -> Result<(), AconfigdError> {
    if dst.exists() {
        set_file_permission(dst, 0o644)?;
    }

    let mut src_file = File::open(src).map_err(|errmsg| AconfigdError::FailToCopyFile {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        errmsg,
    })?;

    let mut dst_file = File::create(dst).map_err(|errmsg| AconfigdError::FailToCopyFile {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        errmsg,
    })?;

    std::io::copy(&mut src_file, &mut dst_file).map_err(|errmsg| {
        AconfigdError::FailToCopyFile {
            src: src.display().to_string(),
            dst: dst.display().to_string(),
            errmsg,
        }
    })?;

    set_file_permission(dst, mode)
}

/// Remove file
pub(crate) fn remove_file(src: &Path) -> Result<(), AconfigdError> {
    if let Ok(true) = src.try_exists() {
        std::fs::remove_file(src).map_err(|errmsg| AconfigdError::FailToRemoveFile {
            file: src.display().to_string(),
            errmsg,
        })?;
    }
    Ok(())
}

/// Read pb from file
pub(crate) fn read_pb_from_file<T: protobuf::Message>(file: &Path) -> Result<T, AconfigdError> {
    if !Path::new(file).exists() {
        return Ok(T::new());
    }

    let data = std::fs::read(file).map_err(|errmsg| AconfigdError::FailToReadFile {
        file: file.display().to_string(),
        errmsg,
    })?;
    protobuf::Message::parse_from_bytes(data.as_ref()).map_err(|errmsg| {
        AconfigdError::FailToParsePbFromBytes { file: file.display().to_string(), errmsg }
    })
}

/// Write pb to file
pub(crate) fn write_pb_to_file<T: protobuf::Message>(
    pb: &T,
    file: &Path,
) -> Result<(), AconfigdError> {
    let bytes = protobuf::Message::write_to_bytes(pb).map_err(|errmsg| {
        AconfigdError::FailToSerializePb { file: file.display().to_string(), errmsg }
    })?;
    std::fs::write(file, bytes).map_err(|errmsg| AconfigdError::FailToWriteFile {
        file: file.display().to_string(),
        errmsg,
    })?;
    Ok(())
}

/// The digest is returned as a hexadecimal string.
pub(crate) fn get_files_digest(paths: &[&Path]) -> Result<String, AconfigdError> {
    let mut hasher = Hasher::new(MessageDigest::sha256())
        .map_err(|errmsg| AconfigdError::FailToGetHasherForDigest { errmsg })?;
    let mut buffer = [0; 1024];
    for path in paths {
        let mut f = File::open(path).map_err(|errmsg| AconfigdError::FailToOpenFile {
            file: path.display().to_string(),
            errmsg,
        })?;
        loop {
            let n = f.read(&mut buffer[..]).map_err(|errmsg| AconfigdError::FailToReadFile {
                file: path.display().to_string(),
                errmsg,
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer).map_err(|errmsg| AconfigdError::FailToHashFile {
                file: path.display().to_string(),
                errmsg,
            })?;
        }
    }
    let digest: &[u8] =
        &hasher.finish().map_err(|errmsg| AconfigdError::FailToGetDigest { errmsg })?;
    let mut xdigest = String::new();
    for x in digest {
        xdigest.push_str(format!("{:02x}", x).as_str());
    }
    Ok(xdigest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aconfigd_protos::ProtoLocalFlagOverrides;
    use std::io::Write;
    use tempfile::tempdir;

    fn get_file_perm_mode(file: &Path) -> u32 {
        let f = std::fs::File::open(&file).unwrap();
        let metadata = f.metadata().unwrap();
        metadata.permissions().mode() & 0o777
    }

    #[test]
    fn test_copy_file() {
        let tmp_dir = tempdir().unwrap();

        let package_map = tmp_dir.path().join("package.map");
        copy_file(&Path::new("./tests/data/package.map"), &package_map, 0o444).unwrap();
        assert_eq!(get_file_perm_mode(&package_map), 0o444);

        let flag_map = tmp_dir.path().join("flag.map");
        copy_file(&Path::new("./tests/data/flag.map"), &flag_map, 0o644).unwrap();
        assert_eq!(get_file_perm_mode(&flag_map), 0o644);
    }

    #[test]
    fn test_remove_file() {
        let tmp_dir = tempdir().unwrap();
        let package_map = tmp_dir.path().join("package.map");
        copy_file(&Path::new("./tests/data/package.map"), &package_map, 0o444).unwrap();
        assert!(remove_file(&package_map).is_ok());
        assert!(!package_map.exists());
    }

    #[test]
    fn test_set_file_permission() {
        let tmp_dir = tempdir().unwrap();
        let package_map = tmp_dir.path().join("package.map");
        copy_file(&Path::new("./tests/data/package.map"), &package_map, 0o644).unwrap();
        set_file_permission(&package_map, 0o444).unwrap();
        assert_eq!(get_file_perm_mode(&package_map), 0o444);
    }

    #[test]
    fn test_write_pb_to_file() {
        let tmp_dir = tempdir().unwrap();
        let test_pb = tmp_dir.path().join("test.pb");
        let pb = ProtoLocalFlagOverrides::new();
        write_pb_to_file(&pb, &test_pb).unwrap();
        assert!(test_pb.exists());
    }

    #[test]
    fn test_read_pb_from_file() {
        let tmp_dir = tempdir().unwrap();
        let test_pb = tmp_dir.path().join("test.pb");
        let pb = ProtoLocalFlagOverrides::new();
        write_pb_to_file(&pb, &test_pb).unwrap();
        let new_pb: ProtoLocalFlagOverrides = read_pb_from_file(&test_pb).unwrap();
        assert_eq!(new_pb.overrides.len(), 0);
    }

    #[test]
    fn test_get_files_digest() {
        let path1 = Path::new("/tmp/hi.txt");
        let path2 = Path::new("/tmp/bye.txt");
        let mut file1 = File::create(path1).unwrap();
        let mut file2 = File::create(path2).unwrap();
        file1.write_all(b"Hello, world!").expect("Writing to file");
        file2.write_all(b"Goodbye, world!").expect("Writing to file");
        let digest = get_files_digest(&[path1, path2]);
        assert_eq!(
            digest.expect("Calculating digest"),
            "8352c31d9ff5f446b838139b7f4eb5fed821a1f80d6648ffa6ed7391ecf431f4"
        );
    }
}
