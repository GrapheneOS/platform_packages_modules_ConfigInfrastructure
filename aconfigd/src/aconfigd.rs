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

use crate::storage_files_manager::StorageFilesManager;
use crate::utils::{read_pb_from_file, remove_file, write_pb_to_file};
use crate::AconfigdError;
use aconfigd_protos::{
    ProtoFlagOverrideMessage, ProtoFlagQueryMessage, ProtoFlagQueryReturnMessage,
    ProtoListStorageMessage, ProtoListStorageMessageMsg, ProtoNewStorageMessage,
    ProtoOTAFlagStagingMessage, ProtoPersistStorageRecords, ProtoRemoveLocalOverrideMessage,
    ProtoStorageRequestMessage, ProtoStorageRequestMessageMsg, ProtoStorageRequestMessages,
    ProtoStorageReturnMessage, ProtoStorageReturnMessages,
};
use log::{debug, error, warn};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

// Aconfigd that is capable of doing both one shot storage file init and socket
// service
#[derive(Debug)]
pub struct Aconfigd {
    pub root_dir: PathBuf,
    pub persist_storage_records: PathBuf,
    pub(crate) storage_manager: StorageFilesManager,
}

impl Aconfigd {
    /// Constructor
    pub fn new(root_dir: &Path, records: &Path) -> Self {
        Self {
            root_dir: root_dir.to_path_buf(),
            persist_storage_records: records.to_path_buf(),
            storage_manager: StorageFilesManager::new(root_dir),
        }
    }

    /// Remove old boot storage record
    pub fn remove_boot_files(&mut self) -> Result<(), AconfigdError> {
        let boot_dir = self.root_dir.join("boot");
        let pb = read_pb_from_file::<ProtoPersistStorageRecords>(&self.persist_storage_records)?;
        for entry in pb.records.iter() {
            debug!("remove boot storage files for container {}", entry.container());
            let boot_value_file = boot_dir.join(entry.container().to_owned() + ".val");
            let boot_info_file = boot_dir.join(entry.container().to_owned() + ".info");
            if boot_value_file.exists() {
                remove_file(&boot_value_file)?;
            }
            if boot_info_file.exists() {
                remove_file(&boot_info_file)?;
            }
        }
        Ok(())
    }

    /// Remove non platform boot storage file copies
    pub fn remove_non_platform_boot_files(&mut self) -> Result<(), AconfigdError> {
        let boot_dir = self.root_dir.join("boot");
        for entry in std::fs::read_dir(&boot_dir)
            .map_err(|errmsg| AconfigdError::FailToReadBootDir { errmsg })?
        {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    if let Some(base_name) = path.file_name() {
                        if let Some(file_name) = base_name.to_str() {
                            if file_name.starts_with("system")
                                || file_name.starts_with("system_ext")
                                || file_name.starts_with("product")
                                || file_name.starts_with("vendor")
                            {
                                continue;
                            }
                            remove_file(&path);
                        }
                    }
                }
                Err(errmsg) => {
                    warn!("failed to visit entry: {}", errmsg);
                }
            }
        }
        Ok(())
    }

    /// Initialize aconfigd from persist storage records
    pub fn initialize_from_storage_record(&mut self) -> Result<(), AconfigdError> {
        let boot_dir = self.root_dir.join("boot");
        let pb = read_pb_from_file::<ProtoPersistStorageRecords>(&self.persist_storage_records)?;
        for entry in pb.records.iter() {
            self.storage_manager.add_storage_files_from_pb(entry);
        }
        Ok(())
    }

    /// Initialize platform storage files, create or update existing persist
    /// storage files and create new boot storage files for each platform
    /// partitions
    pub fn initialize_platform_storage(&mut self) -> Result<(), AconfigdError> {
        for container in ["system", "system_ext", "product", "vendor"] {
            debug!("start initialize {} flags", container);

            let aconfig_dir = PathBuf::from("/".to_string() + container + "/etc/aconfig");
            let default_package_map = aconfig_dir.join("package.map");
            let default_flag_map = aconfig_dir.join("flag.map");
            let default_flag_val = aconfig_dir.join("flag.val");
            let default_flag_info = aconfig_dir.join("flag.info");

            if !default_package_map.exists()
                || !default_flag_map.exists()
                || !default_flag_val.exists()
                || !default_flag_info.exists()
            {
                debug!("skip {} initialization due to missing storage files", container);
                continue;
            }

            if std::fs::metadata(&default_flag_val)
                .map_err(|errmsg| AconfigdError::FailToGetFileMetadata {
                    file: default_flag_val.display().to_string(),
                    errmsg,
                })?
                .len()
                == 0
            {
                debug!("skip {} initialization due to zero sized storage files", container);
                continue;
            }

            self.storage_manager.add_or_update_container_storage_files(
                container,
                &default_package_map,
                &default_flag_map,
                &default_flag_val,
                &default_flag_info,
            )?;

            self.storage_manager
                .write_persist_storage_records_to_file(&self.persist_storage_records)?;
        }

        self.storage_manager.apply_staged_ota_flags()?;

        for container in ["system", "system_ext", "product", "vendor"] {
            self.storage_manager.apply_all_staged_overrides(container)?;
        }

        Ok(())
    }

    /// Initialize mainline storage files, create or update existing persist
    /// storage files and create new boot storage files for each mainline
    /// container
    pub fn initialize_mainline_storage(&mut self) -> Result<(), AconfigdError> {
        // get all the apex dirs to visit
        let mut dirs_to_visit = Vec::new();
        let apex_dir = PathBuf::from("/apex");
        for entry in std::fs::read_dir(&apex_dir)
            .map_err(|errmsg| AconfigdError::FailToReadApexDir { errmsg })?
        {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    if let Some(base_name) = path.file_name() {
                        if let Some(dir_name) = base_name.to_str() {
                            if dir_name.starts_with('.') {
                                continue;
                            }
                            if dir_name.find('@').is_some() {
                                continue;
                            }
                            if dir_name == "sharedlibs" {
                                continue;
                            }
                            dirs_to_visit.push(dir_name.to_string());
                        }
                    }
                }
                Err(errmsg) => {
                    warn!("failed to visit entry: {}", errmsg);
                }
            }
        }

        // initialize each container
        for container in dirs_to_visit.iter() {
            debug!("start initialize {} flags", container);
            let etc_dir = apex_dir.join(container).join("etc");
            let default_package_map = etc_dir.join("package.map");
            let default_flag_map = etc_dir.join("flag.map");
            let default_flag_val = etc_dir.join("flag.val");
            let default_flag_info = etc_dir.join("flag.info");

            if !default_package_map.exists()
                || !default_flag_val.exists()
                || !default_flag_val.exists()
                || !default_flag_map.exists()
            {
                continue;
            }

            if std::fs::metadata(&default_flag_val)
                .map_err(|errmsg| AconfigdError::FailToGetFileMetadata {
                    file: default_flag_val.display().to_string(),
                    errmsg,
                })?
                .len()
                == 0
            {
                continue;
            }

            self.storage_manager.add_or_update_container_storage_files(
                container,
                &default_package_map,
                &default_flag_map,
                &default_flag_val,
                &default_flag_info,
            )?;

            self.storage_manager
                .write_persist_storage_records_to_file(&self.persist_storage_records)?;

            self.storage_manager.apply_all_staged_overrides(container)?;
        }

        Ok(())
    }

    /// Handle a flag override request
    fn handle_flag_override(
        &mut self,
        request_pb: &ProtoFlagOverrideMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        self.storage_manager.override_flag_value(
            request_pb.package_name(),
            request_pb.flag_name(),
            request_pb.flag_value(),
            request_pb.override_type(),
        )?;
        let mut return_pb = ProtoStorageReturnMessage::new();
        return_pb.mut_flag_override_message();
        Ok(return_pb)
    }

    /// Handle ota flag staging request
    fn handle_ota_staging(
        &mut self,
        request_pb: &ProtoOTAFlagStagingMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        let ota_flags_pb_file = self.root_dir.join("flags").join("ota.pb");

        let mut existing_ota_flags =
            read_pb_from_file::<ProtoOTAFlagStagingMessage>(&ota_flags_pb_file)
                .unwrap_or_else(|_| ProtoOTAFlagStagingMessage::new());

        if request_pb.has_build_id()
            && existing_ota_flags.has_build_id()
            && request_pb.build_id() == existing_ota_flags.build_id()
        {
            let mut merged_flags = existing_ota_flags.overrides.to_vec();
            merged_flags.extend(request_pb.overrides.clone());

            let mut seen_flags = std::collections::HashSet::new();
            let mut new_flags = Vec::new();

            for flag in merged_flags {
                let flag = flag.clone();
                let package_name = flag.package_name().to_string();
                let flag_name = flag.flag_name().to_string();
                let key = (package_name, flag_name);
                if seen_flags.insert(key) {
                    new_flags.push(flag);
                }
            }
            merged_flags = new_flags;
            existing_ota_flags.overrides = merged_flags.into();

            write_pb_to_file::<ProtoOTAFlagStagingMessage>(
                &existing_ota_flags,
                &ota_flags_pb_file,
            )?;
        } else {
            write_pb_to_file::<ProtoOTAFlagStagingMessage>(request_pb, &ota_flags_pb_file)?;
        }

        let mut return_pb = ProtoStorageReturnMessage::new();
        return_pb.mut_ota_staging_message();
        Ok(return_pb)
    }

    /// Handle new container storage request
    fn handle_new_storage(
        &mut self,
        request_pb: &ProtoNewStorageMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        self.storage_manager.add_or_update_container_storage_files(
            request_pb.container(),
            Path::new(request_pb.package_map()),
            Path::new(request_pb.flag_map()),
            Path::new(request_pb.flag_value()),
            Path::new(request_pb.flag_info()),
        )?;

        self.storage_manager
            .write_persist_storage_records_to_file(&self.persist_storage_records)?;
        self.storage_manager.apply_all_staged_overrides(request_pb.container())?;

        let mut return_pb = ProtoStorageReturnMessage::new();
        return_pb.mut_new_storage_message();
        Ok(return_pb)
    }

    /// Handle flag query request
    fn handle_flag_query(
        &mut self,
        request_pb: &ProtoFlagQueryMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        let mut return_pb = ProtoStorageReturnMessage::new();
        match self
            .storage_manager
            .get_flag_snapshot(request_pb.package_name(), request_pb.flag_name())?
        {
            Some(snapshot) => {
                let result = return_pb.mut_flag_query_message();
                result.set_container(snapshot.container);
                result.set_package_name(snapshot.package);
                result.set_flag_name(snapshot.flag);
                result.set_server_flag_value(snapshot.server_value);
                result.set_local_flag_value(snapshot.local_value);
                result.set_boot_flag_value(snapshot.boot_value);
                result.set_default_flag_value(snapshot.default_value);
                result.set_is_readwrite(snapshot.is_readwrite);
                result.set_has_server_override(snapshot.has_server_override);
                result.set_has_local_override(snapshot.has_local_override);
                Ok(return_pb)
            }
            None => Err(AconfigdError::FlagDoesNotExist {
                flag: request_pb.package_name().to_string() + "." + request_pb.flag_name(),
            }),
        }
    }

    /// Handle local override removal request
    fn handle_local_override_removal(
        &mut self,
        request_pb: &ProtoRemoveLocalOverrideMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        if request_pb.remove_all() {
            self.storage_manager.remove_all_local_overrides()?;
        } else {
            self.storage_manager.remove_local_override(
                request_pb.package_name(),
                request_pb.flag_name(),
                request_pb.remove_override_type(),
            )?;
        }
        let mut return_pb = ProtoStorageReturnMessage::new();
        return_pb.mut_remove_local_override_message();
        Ok(return_pb)
    }

    /// Handle storage reset request
    fn handle_storage_reset(&mut self) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        self.storage_manager.reset_all_storage()?;
        let mut return_pb = ProtoStorageReturnMessage::new();
        return_pb.mut_reset_storage_message();
        Ok(return_pb)
    }

    /// Handle list storage request
    fn handle_list_storage(
        &mut self,
        request_pb: &ProtoListStorageMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        let flags = match &request_pb.msg {
            Some(ProtoListStorageMessageMsg::All(_)) => self.storage_manager.list_all_flags(),
            Some(ProtoListStorageMessageMsg::Container(container)) => {
                self.storage_manager.list_flags_in_container(container)
            }
            Some(ProtoListStorageMessageMsg::PackageName(package)) => {
                self.storage_manager.list_flags_in_package(package)
            }
            _ => Err(AconfigdError::InvalidSocketRequest {
                errmsg: "Invalid list storage type".to_string(),
            }),
        }?;
        let mut return_pb = ProtoStorageReturnMessage::new();
        let result = return_pb.mut_list_storage_message();
        result.flags = flags
            .into_iter()
            .map(|f| {
                let mut snapshot = ProtoFlagQueryReturnMessage::new();
                snapshot.set_container(f.container);
                snapshot.set_package_name(f.package);
                snapshot.set_flag_name(f.flag);
                snapshot.set_server_flag_value(f.server_value);
                snapshot.set_local_flag_value(f.local_value);
                snapshot.set_boot_flag_value(f.boot_value);
                snapshot.set_default_flag_value(f.default_value);
                snapshot.set_is_readwrite(f.is_readwrite);
                snapshot.set_has_server_override(f.has_server_override);
                snapshot.set_has_local_override(f.has_local_override);
                snapshot.set_has_boot_local_override(f.has_boot_local_override);
                snapshot
            })
            .collect();
        Ok(return_pb)
    }

    /// Handle socket request
    fn handle_socket_request(
        &mut self,
        request_pb: &ProtoStorageRequestMessage,
    ) -> Result<ProtoStorageReturnMessage, AconfigdError> {
        match request_pb.msg {
            Some(ProtoStorageRequestMessageMsg::NewStorageMessage(_)) => {
                self.handle_new_storage(request_pb.new_storage_message())
            }
            Some(ProtoStorageRequestMessageMsg::FlagOverrideMessage(_)) => {
                self.handle_flag_override(request_pb.flag_override_message())
            }
            Some(ProtoStorageRequestMessageMsg::OtaStagingMessage(_)) => {
                self.handle_ota_staging(request_pb.ota_staging_message())
            }
            Some(ProtoStorageRequestMessageMsg::FlagQueryMessage(_)) => {
                self.handle_flag_query(request_pb.flag_query_message())
            }
            Some(ProtoStorageRequestMessageMsg::RemoveLocalOverrideMessage(_)) => {
                self.handle_local_override_removal(request_pb.remove_local_override_message())
            }
            Some(ProtoStorageRequestMessageMsg::ResetStorageMessage(_)) => {
                self.handle_storage_reset()
            }
            Some(ProtoStorageRequestMessageMsg::ListStorageMessage(_)) => {
                self.handle_list_storage(request_pb.list_storage_message())
            }
            _ => Err(AconfigdError::InvalidSocketRequest { errmsg: String::new() }),
        }
    }

    /// Handle socket request from a unix stream
    pub fn handle_socket_request_from_stream(
        &mut self,
        stream: &mut UnixStream,
    ) -> Result<(), AconfigdError> {
        let mut length_buffer = [0u8; 4];
        stream
            .read_exact(&mut length_buffer)
            .map_err(|errmsg| AconfigdError::FailToReadFromSocket { errmsg })?;
        let mut message_length = u32::from_be_bytes(length_buffer);

        let mut request_buffer = vec![0u8; message_length as usize];
        stream
            .read_exact(&mut request_buffer)
            .map_err(|errmsg| AconfigdError::FailToReadFromSocket { errmsg })?;

        let requests: &ProtoStorageRequestMessages =
            &protobuf::Message::parse_from_bytes(&request_buffer[..]).map_err(|errmsg| {
                AconfigdError::FailToParsePbFromBytes { file: "socket request".to_string(), errmsg }
            })?;

        let mut return_msgs = ProtoStorageReturnMessages::new();
        for request in requests.msgs.iter() {
            let return_pb = match self.handle_socket_request(request) {
                Ok(return_msg) => return_msg,
                Err(errmsg) => {
                    error!("failed to handle socket request: {}", errmsg);
                    let mut return_msg = ProtoStorageReturnMessage::new();
                    return_msg.set_error_message(format!(
                        "failed to handle socket request: {:?}",
                        errmsg
                    ));
                    return_msg
                }
            };
            return_msgs.msgs.push(return_pb);
        }

        let bytes = protobuf::Message::write_to_bytes(&return_msgs).map_err(|errmsg| {
            AconfigdError::FailToSerializePb { file: "socket".to_string(), errmsg }
        })?;

        message_length = bytes.len() as u32;
        length_buffer = message_length.to_be_bytes();
        stream
            .write_all(&length_buffer)
            .map_err(|errmsg| AconfigdError::FailToWriteToSocket { errmsg })?;
        stream.write_all(&bytes).map_err(|errmsg| AconfigdError::FailToWriteToSocket { errmsg })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{has_same_content, ContainerMock, StorageRootDirMock};
    use crate::utils::{get_files_digest, read_pb_from_file};
    use aconfigd_protos::{
        ProtoFlagOverride, ProtoFlagOverrideType, ProtoLocalFlagOverrides,
        ProtoPersistStorageRecord,
    };
    use std::net::Shutdown;
    use std::os::fd::{FromRawFd, IntoRawFd, RawFd};
    use tempfile::tempfile;

    fn create_mock_aconfigd(root_dir: &StorageRootDirMock) -> Aconfigd {
        Aconfigd::new(root_dir.tmp_dir.path(), &root_dir.flags_dir.join("storage_records.pb"))
    }

    fn add_mockup_container_storage(container: &ContainerMock, aconfigd: &mut Aconfigd) {
        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_new_storage_message();
        actual_request.set_container("mockup".to_string());
        actual_request.set_package_map(container.package_map.display().to_string());
        actual_request.set_flag_map(container.flag_map.display().to_string());
        actual_request.set_flag_value(container.flag_val.display().to_string());
        actual_request.set_flag_info(container.flag_info.display().to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());
    }

    #[test]
    fn test_new_storage_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);

        let persist_package_map = root_dir.maps_dir.join("mockup.package.map");
        assert!(persist_package_map.exists());
        assert!(has_same_content(&container.package_map, &persist_package_map));
        let persist_flag_map = root_dir.maps_dir.join("mockup.flag.map");
        assert!(persist_flag_map.exists());
        assert!(has_same_content(&container.flag_map, &persist_flag_map));
        let persist_flag_val = root_dir.flags_dir.join("mockup.val");
        assert!(persist_flag_val.exists());
        assert!(has_same_content(&container.flag_val, &persist_flag_val));
        let persist_flag_info = root_dir.flags_dir.join("mockup.info");
        assert!(persist_flag_info.exists());
        assert!(has_same_content(&container.flag_info, &persist_flag_info));
        let boot_flag_val = root_dir.boot_dir.join("mockup.val");
        assert!(boot_flag_val.exists());
        assert!(has_same_content(&container.flag_val, &boot_flag_val));
        let boot_flag_info = root_dir.boot_dir.join("mockup.info");
        assert!(boot_flag_info.exists());
        assert!(has_same_content(&container.flag_info, &boot_flag_info));

        let digest = get_files_digest(
            &[
                container.package_map.as_path(),
                container.flag_map.as_path(),
                container.flag_val.as_path(),
                container.flag_info.as_path(),
            ][..],
        )
        .unwrap();
        let pb = read_pb_from_file::<ProtoPersistStorageRecords>(&aconfigd.persist_storage_records)
            .unwrap();
        assert_eq!(pb.records.len(), 1);
        let mut entry = ProtoPersistStorageRecord::new();
        entry.set_version(1);
        entry.set_container("mockup".to_string());
        entry.set_package_map(container.package_map.display().to_string());
        entry.set_flag_map(container.flag_map.display().to_string());
        entry.set_flag_val(container.flag_val.display().to_string());
        entry.set_flag_info(container.flag_info.display().to_string());
        entry.set_digest(digest);
        assert_eq!(pb.records[0], entry);
    }

    fn get_flag_snapshot(
        aconfigd: &mut Aconfigd,
        package: &str,
        flag: &str,
    ) -> ProtoFlagQueryReturnMessage {
        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_query_message();
        actual_request.set_package_name(package.to_string());
        actual_request.set_flag_name(flag.to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());
        return_msg.unwrap().flag_query_message().clone()
    }

    #[test]
    fn test_server_on_boot_flag_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::SERVER_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.server_flag_value(), "false");
        assert_eq!(flag.boot_flag_value(), "true");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.has_server_override(), true);
        assert_eq!(flag.has_local_override(), false);
    }

    #[test]
    fn test_local_on_boot_flag_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.server_flag_value(), "");
        assert_eq!(flag.boot_flag_value(), "true");
        assert_eq!(flag.local_flag_value(), "false");
        assert_eq!(flag.has_server_override(), false);
        assert_eq!(flag.has_local_override(), true);
    }

    #[test]
    fn test_local_immediate_flag_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_IMMEDIATE);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.server_flag_value(), "");
        assert_eq!(flag.boot_flag_value(), "false");
        assert_eq!(flag.local_flag_value(), "false");
        assert_eq!(flag.has_server_override(), false);
        assert_eq!(flag.has_local_override(), true);
    }

    #[test]
    fn test_negative_flag_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("not_exist".to_string());
        actual_request.set_flag_name("not_exist".to_string());
        actual_request.set_flag_value("false".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_err());
        if let Err(errmsg) = return_msg {
            assert_eq!("cannot find container for package not_exist", format!("{}", errmsg));
        }
    }

    #[test]
    fn test_ota_flag_staging_request() {
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_ota_staging_message();
        actual_request.set_build_id("xyz.123".to_string());
        let mut flag1 = ProtoFlagOverride::new();
        flag1.set_package_name("package_foo".to_string());
        flag1.set_flag_name("flag_foo".to_string());
        flag1.set_flag_value("false".to_string());
        actual_request.overrides.push(flag1.clone());
        let mut flag2 = ProtoFlagOverride::new();
        flag2.set_package_name("package_bar".to_string());
        flag2.set_flag_name("flag_bar".to_string());
        flag2.set_flag_value("true".to_string());
        actual_request.overrides.push(flag2.clone());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let ota_pb_file = root_dir.flags_dir.join("ota.pb");
        assert!(ota_pb_file.exists());
        let ota_flags = read_pb_from_file::<ProtoOTAFlagStagingMessage>(&ota_pb_file).unwrap();
        assert_eq!(ota_flags.build_id(), "xyz.123");
        assert_eq!(ota_flags.overrides.len(), 2);
        assert_eq!(ota_flags.overrides[0], flag1);
        assert_eq!(ota_flags.overrides[1], flag2);
    }

    #[test]
    fn test_flag_querry_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.container(), "mockup");
        assert_eq!(flag.package_name(), "com.android.aconfig.storage.test_1");
        assert_eq!(flag.flag_name(), "enabled_rw");
        assert_eq!(flag.server_flag_value(), "");
        assert_eq!(flag.boot_flag_value(), "true");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.default_flag_value(), "true");
        assert_eq!(flag.is_readwrite(), true);
        assert_eq!(flag.has_server_override(), false);
        assert_eq!(flag.has_local_override(), false);

        let mut request = ProtoStorageRequestMessage::new();
        let mut actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::SERVER_ON_REBOOT);
        let mut return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        flag = get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.container(), "mockup");
        assert_eq!(flag.package_name(), "com.android.aconfig.storage.test_1");
        assert_eq!(flag.flag_name(), "enabled_rw");
        assert_eq!(flag.server_flag_value(), "false");
        assert_eq!(flag.boot_flag_value(), "true");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.default_flag_value(), "true");
        assert_eq!(flag.is_readwrite(), true);
        assert_eq!(flag.has_server_override(), true);
        assert_eq!(flag.has_local_override(), false);

        request = ProtoStorageRequestMessage::new();
        actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_IMMEDIATE);
        return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        flag = get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.container(), "mockup");
        assert_eq!(flag.package_name(), "com.android.aconfig.storage.test_1");
        assert_eq!(flag.flag_name(), "enabled_rw");
        assert_eq!(flag.server_flag_value(), "false");
        assert_eq!(flag.boot_flag_value(), "false");
        assert_eq!(flag.local_flag_value(), "false");
        assert_eq!(flag.default_flag_value(), "true");
        assert_eq!(flag.is_readwrite(), true);
        assert_eq!(flag.has_server_override(), true);
        assert_eq!(flag.has_local_override(), true);
    }

    #[test]
    fn test_negative_flag_querry_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_query_message();
        actual_request.set_package_name("not_exist".to_string());
        actual_request.set_flag_name("not_exist".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_err());
        if let Err(errmsg) = return_msg {
            assert_eq!("flag not_exist.not_exist does not exist", format!("{}", errmsg));
        }
    }

    #[test]
    fn test_remove_single_local_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("disabled_rw".to_string());
        actual_request.set_flag_value("true".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_remove_local_override_message();
        actual_request.set_remove_all(false);
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.has_local_override(), false);

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "disabled_rw");
        assert_eq!(flag.local_flag_value(), "true");
        assert_eq!(flag.has_local_override(), true);
    }

    #[test]
    fn test_remove_all_local_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("disabled_rw".to_string());
        actual_request.set_flag_value("true".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_remove_local_override_message();
        actual_request.set_remove_all(true);
        actual_request.set_package_name("abc".to_string());
        actual_request.set_flag_name("def".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.has_local_override(), false);

        let flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "disabled_rw");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.has_local_override(), false);

        let local_pb_file = root_dir.flags_dir.join("mockup_local_overrides.pb");
        let pb = read_pb_from_file::<ProtoLocalFlagOverrides>(&local_pb_file).unwrap();
        assert_eq!(pb.overrides.len(), 0);
    }

    #[test]
    fn test_negative_remove_local_override_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_remove_local_override_message();
        actual_request.set_remove_all(false);
        actual_request.set_package_name("abc".to_string());
        actual_request.set_flag_name("def".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_err());
        if let Err(errmsg) = return_msg {
            assert_eq!("cannot find container for package abc", format!("{}", errmsg));
        }
    }

    #[test]
    fn test_reset_storage_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("enabled_rw".to_string());
        actual_request.set_flag_value("false".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::SERVER_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_override_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        actual_request.set_flag_name("disabled_rw".to_string());
        actual_request.set_flag_value("true".to_string());
        actual_request.set_override_type(ProtoFlagOverrideType::LOCAL_ON_REBOOT);
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let mut request = ProtoStorageRequestMessage::new();
        let _actual_request = request.mut_reset_storage_message();
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let mut flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "enabled_rw");
        assert_eq!(flag.server_flag_value(), "");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.has_server_override(), false);
        assert_eq!(flag.has_local_override(), false);

        flag =
            get_flag_snapshot(&mut aconfigd, "com.android.aconfig.storage.test_1", "disabled_rw");
        assert_eq!(flag.server_flag_value(), "");
        assert_eq!(flag.local_flag_value(), "");
        assert_eq!(flag.has_server_override(), false);
        assert_eq!(flag.has_local_override(), false);
    }

    #[test]
    fn test_list_package_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_list_storage_message();
        actual_request.set_package_name("com.android.aconfig.storage.test_1".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flags = return_msg.unwrap().list_storage_message().clone();
        assert_eq!(flags.flags.len(), 3);

        let mut flag = ProtoFlagQueryReturnMessage::new();
        flag.set_container("mockup".to_string());
        flag.set_package_name("com.android.aconfig.storage.test_1".to_string());
        flag.set_flag_name("disabled_rw".to_string());
        flag.set_server_flag_value("".to_string());
        flag.set_local_flag_value("".to_string());
        flag.set_boot_flag_value("false".to_string());
        flag.set_default_flag_value("false".to_string());
        flag.set_is_readwrite(true);
        flag.set_has_server_override(false);
        flag.set_has_local_override(false);
        assert_eq!(flags.flags[0], flag);

        flag.set_flag_name("enabled_ro".to_string());
        flag.set_boot_flag_value("true".to_string());
        flag.set_default_flag_value("true".to_string());
        flag.set_is_readwrite(false);
        assert_eq!(flags.flags[1], flag);

        flag.set_flag_name("enabled_rw".to_string());
        flag.set_is_readwrite(true);
        assert_eq!(flags.flags[2], flag);
    }

    #[test]
    fn test_negative_list_package_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_list_storage_message();
        actual_request.set_package_name("not_exist".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_err());
        if let Err(errmsg) = return_msg {
            assert_eq!("cannot find container for package not_exist", format!("{}", errmsg));
        }
    }

    #[test]
    fn test_list_container_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_list_storage_message();
        actual_request.set_container("mockup".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_ok());

        let flags = return_msg.unwrap().list_storage_message().clone();
        assert_eq!(flags.flags.len(), 8);
    }

    #[test]
    fn test_negative_list_container_request() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_list_storage_message();
        actual_request.set_container("not_exist".to_string());
        let return_msg = aconfigd.handle_socket_request(&request);
        assert!(return_msg.is_err());
        if let Err(errmsg) = return_msg {
            assert_eq!("fail to get storage files for not_exist", format!("{}", errmsg));
        }
    }

    #[test]
    fn test_aconfigd_unix_stream() {
        let container = ContainerMock::new();
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        add_mockup_container_storage(&container, &mut aconfigd);
        aconfigd.storage_manager.apply_all_staged_overrides("mockup").unwrap();

        let mut request = ProtoStorageRequestMessage::new();
        let actual_request = request.mut_flag_query_message();
        actual_request.set_package_name("abc".to_string());
        actual_request.set_flag_name("def".to_string());
        let bytes = protobuf::Message::write_to_bytes(&request).unwrap();

        let (mut stream1, mut stream2) = UnixStream::pair().unwrap();
        let length_bytes = (bytes.len() as u32).to_be_bytes();
        stream1.write_all(&length_bytes).unwrap();
        stream1.write_all(&bytes).unwrap();
        stream1.shutdown(Shutdown::Write).unwrap();
        let result = aconfigd.handle_socket_request_from_stream(&mut stream2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_negative_aconfigd_unix_stream() {
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);

        let (mut stream1, mut stream2) = UnixStream::pair().unwrap();
        let length_bytes = 11_u32.to_be_bytes();
        stream1.write_all(&length_bytes).unwrap();
        stream1.write_all(b"hello world").unwrap();
        stream1.shutdown(Shutdown::Write).unwrap();
        let result = aconfigd.handle_socket_request_from_stream(&mut stream2);
        assert!(result.is_err());
        if let Err(errmsg) = result {
            assert_eq!("fail to parse to protobuf from bytes for socket request: Error(WireError(UnexpectedWireType(EndGroup)))", format!("{}", errmsg));
        }
    }

    #[test]
    fn test_initialize_platform_storage_fresh_install() {
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        aconfigd.initialize_platform_storage().unwrap();
        assert!(aconfigd.persist_storage_records.exists());
        let pb = read_pb_from_file::<ProtoPersistStorageRecords>(&aconfigd.persist_storage_records)
            .unwrap();
        assert_eq!(pb.records.len(), 3);

        for container in ["system", "system_ext", "product", "vendor"] {
            let aconfig_dir = PathBuf::from("/".to_string() + container + "/etc/aconfig");
            let default_package_map = aconfig_dir.join("package.map");
            let default_flag_map = aconfig_dir.join("flag.map");
            let default_flag_val = aconfig_dir.join("flag.val");
            let default_flag_info = aconfig_dir.join("flag.info");

            let persist_package_map =
                root_dir.maps_dir.join(container.to_string() + ".package.map");
            let persist_flag_map = root_dir.maps_dir.join(container.to_string() + ".flag.map");
            let persist_flag_val = root_dir.flags_dir.join(container.to_string() + ".val");
            let persist_flag_info = root_dir.flags_dir.join(container.to_string() + ".info");
            let boot_flag_val = root_dir.boot_dir.join(container.to_string() + ".val");
            let boot_flag_info = root_dir.boot_dir.join(container.to_string() + ".info");
            let local_overrides =
                root_dir.flags_dir.join(container.to_string() + "_local_overrides.pb");

            assert!(has_same_content(&persist_package_map, &default_package_map));
            assert!(has_same_content(&persist_flag_map, &default_flag_map));
            assert!(has_same_content(&persist_flag_val, &default_flag_val));
            assert!(has_same_content(&persist_flag_info, &default_flag_info));
            assert!(has_same_content(&boot_flag_val, &default_flag_val));
            assert!(has_same_content(&boot_flag_info, &default_flag_info));
            assert!(local_overrides.exists());

            let mut entry = ProtoPersistStorageRecord::new();
            entry.set_version(1);
            entry.set_container(container.to_string());
            entry.set_package_map(default_package_map.display().to_string());
            entry.set_flag_map(default_flag_map.display().to_string());
            entry.set_flag_val(default_flag_val.display().to_string());
            entry.set_flag_info(default_flag_info.display().to_string());
            let digest = get_files_digest(
                &[
                    default_package_map.as_path(),
                    default_flag_map.as_path(),
                    default_flag_val.as_path(),
                    default_flag_info.as_path(),
                ][..],
            )
            .unwrap();
            entry.set_digest(digest);
            assert!(pb.records.iter().any(|x| *x == entry));
        }
    }

    #[test]
    fn test_initialize_mainline_storage() {
        let root_dir = StorageRootDirMock::new();
        let mut aconfigd = create_mock_aconfigd(&root_dir);
        aconfigd.initialize_mainline_storage().unwrap();
        let entries: Vec<_> = std::fs::read_dir(&root_dir.flags_dir).into_iter().collect();
        assert!(entries.len() > 0);
    }
}
