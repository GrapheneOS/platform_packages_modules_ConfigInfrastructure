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

use aconfigd_rust::aconfigd::Aconfigd;
use anyhow::{bail, Result};
use log::{debug, error};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::Path;

const ACONFIGD_SOCKET: &str = "aconfigd_mainline";
const ACONFIGD_ROOT_DIR: &str = "/metadata/aconfig";
const STORAGE_RECORDS: &str = "/metadata/aconfig/mainline_storage_records.pb";
const ACONFIGD_SOCKET_BACKLOG: i32 = 8;

/// start aconfigd socket service
pub fn start_socket() -> Result<()> {
    let fd = rustutils::sockets::android_get_control_socket(ACONFIGD_SOCKET)?;

    // SAFETY: Safe because this doesn't modify any memory and we check the return value.
    let ret = unsafe { libc::listen(fd.as_raw_fd(), ACONFIGD_SOCKET_BACKLOG) };
    if ret < 0 {
        bail!(std::io::Error::last_os_error());
    }

    let listener = UnixListener::from(fd);

    let mut aconfigd = Aconfigd::new(Path::new(ACONFIGD_ROOT_DIR), Path::new(STORAGE_RECORDS));
    aconfigd.initialize_from_storage_record()?;

    debug!("start waiting for a new client connection through socket.");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(errmsg) = aconfigd.handle_socket_request_from_stream(&mut stream) {
                    error!("failed to handle socket request: {:?}", errmsg);
                }
            }
            Err(errmsg) => {
                error!("failed to listen for an incoming message: {:?}", errmsg);
            }
        }
    }

    Ok(())
}

/// initialize mainline module storage files
pub fn init() -> Result<()> {
    let mut aconfigd = Aconfigd::new(Path::new(ACONFIGD_ROOT_DIR), Path::new(STORAGE_RECORDS));
    aconfigd.remove_boot_files()?;

    // One time clean up to remove the boot value and info file for mainline modules
    // that are not in the pb records file. For those mainline modules already in the
    // records pb file, the above code should remove their boot copy already. Here
    // we add additional enforcement to ensure that we clear up all mainline boot
    // copies, regardless if a mainline module is in records or not.
    // NOTE: this is a one time operation to be removed once the flag is finalized.
    // as we will add the second change that block boot copy from inactive container
    // to be generated in the first place.
    if aconfig_new_storage_flags::bluetooth_flag_value_bug_fix() {
        aconfigd.remove_non_platform_boot_files()?
    }

    aconfigd.initialize_from_storage_record()?;
    aconfigd.initialize_mainline_storage()?;
    Ok(())
}

/// initialize bootstrapped mainline module storage files
pub fn bootstrap_init() -> Result<()> {
    Ok(())
}
