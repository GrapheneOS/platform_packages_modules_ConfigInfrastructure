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

//! `aconfig_mainline` is a crate that defines library functions that are needed by
//! aconfig daemon for mainline (aconfigd-mainline binary).

pub mod aconfigd;
pub mod storage_files;
pub mod storage_files_manager;
pub mod utils;

#[cfg(test)]
mod test_utils;

/// aconfigd-mainline error
#[non_exhaustive]
#[derive(thiserror::Error, Debug)]
pub enum AconfigdError {
    #[error("failed to update file permission of {} to {}: {:?}", .file, .mode, .errmsg)]
    FailToUpdateFilePerm { file: String, mode: u32, errmsg: std::io::Error },

    #[error("failed to copy file from {} to {}: {:?}", .src, .dst, .errmsg)]
    FailToCopyFile { src: String, dst: String, errmsg: std::io::Error },

    #[error("fail to remove file {}: {:?}", .file, .errmsg)]
    FailToRemoveFile { file: String, errmsg: std::io::Error },

    #[error("fail to open file {}: {:?}", .file, .errmsg)]
    FailToOpenFile { file: String, errmsg: std::io::Error },

    #[error("fail to read file {}: {:?}", .file, .errmsg)]
    FailToReadFile { file: String, errmsg: std::io::Error },

    #[error("fail to write file {}: {:?}", .file, .errmsg)]
    FailToWriteFile { file: String, errmsg: std::io::Error },

    #[error("fail to parse to protobuf from bytes for {}: {:?}", .file, .errmsg)]
    FailToParsePbFromBytes { file: String, errmsg: protobuf::Error },

    #[error("fail to serialize protobuf to bytes for file {}: {:?}", .file, .errmsg)]
    FailToSerializePb { file: String, errmsg: protobuf::Error },

    #[error("fail to get hasher for digest: {:?}", .errmsg)]
    FailToGetHasherForDigest { errmsg: openssl::error::ErrorStack },

    #[error("failed to hash file {}: {:?}", .file, .errmsg)]
    FailToHashFile { file: String, errmsg: openssl::error::ErrorStack },

    #[error("failed to get files digest: {:?}", .errmsg)]
    FailToGetDigest { errmsg: openssl::error::ErrorStack },

    #[error("fail to get storage file version of {}: {:?}", .file, .errmsg)]
    FailToGetFileVersion { file: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("fail to map storage file {}: {:?}", .file, .errmsg)]
    FailToMapFile { file: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("mapped storage file {} is none", .file)]
    MappedFileIsNone { file: String },

    #[error("invalid flag value type for {}: {:?}", .flag, .errmsg)]
    InvalidFlagValueType { flag: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("flag {} does not exist", .flag)]
    FlagDoesNotExist { flag: String },

    #[error("fail to get package context for {}: {:?}", .package, .errmsg)]
    FailToGetPackageContext { package: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("fail to get flag context for {}: {:?}", .flag, .errmsg)]
    FailToGetFlagContext { flag: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("fail to get flag attribute for {}: {:?}", .flag, .errmsg)]
    FailToGetFlagAttribute { flag: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("fail to get flag value for {}: {:?}", .flag, .errmsg)]
    FailToGetFlagValue { flag: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("flag {} has no local override", .flag)]
    FlagHasNoLocalOverride { flag: String },

    #[error("invalid flag value {} for flag {}", .value, .flag)]
    InvalidFlagValue { flag: String, value: String },

    #[error("failed to set flag value for flag {}: {:?}", .flag, .errmsg)]
    FailToSetFlagValue { flag: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("failed to set flag has server override for flag {}: {:?}", .flag, .errmsg)]
    FailToSetFlagHasServerOverride {
        flag: String,
        errmsg: aconfig_storage_file::AconfigStorageError,
    },

    #[error("failed to set flag has local override for flag {}: {:?}", .flag, .errmsg)]
    FailToSetFlagHasLocalOverride {
        flag: String,
        errmsg: aconfig_storage_file::AconfigStorageError,
    },

    #[error("flag {} is readonly", .flag)]
    FlagIsReadOnly { flag: String },

    #[error("fail to list flags for cotnainer {}: {:?}", .container, .errmsg)]
    FailToListFlags { container: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("fail to list flags with info for container {}: {:?}", .container, .errmsg)]
    FailToListFlagsWithInfo { container: String, errmsg: aconfig_storage_file::AconfigStorageError },

    #[error("fail to get storage files for {}", .container)]
    FailToGetStorageFiles { container: String },

    #[error("unexpected internal error")]
    InternalError(#[source] anyhow::Error),

    #[error("fail to get metadata of file {}: {:?}", .file, .errmsg)]
    FailToGetFileMetadata { file: String, errmsg: std::io::Error },

    #[error("fail to read /apex dir: {:?}", .errmsg)]
    FailToReadApexDir { errmsg: std::io::Error },

    #[error("fail to read /boot dir: {:?}", .errmsg)]
    FailToReadBootDir { errmsg: std::io::Error },

    #[error("cannot find container for package {}", .package)]
    FailToFindContainer { package: String },

    #[error("invalid socket request: {}", .errmsg)]
    InvalidSocketRequest { errmsg: String },

    #[error("fail to read from socket unix stream: {:?}", .errmsg)]
    FailToReadFromSocket { errmsg: std::io::Error },

    #[error("fail to write to socket unix stream: {:?}", .errmsg)]
    FailToWriteToSocket { errmsg: std::io::Error },

    #[error("fail to read device build fingerpirnt: {:?}", .errmsg)]
    FailToReadBuildFingerPrint { errmsg: rustutils::system_properties::PropertyWatcherError },
}
