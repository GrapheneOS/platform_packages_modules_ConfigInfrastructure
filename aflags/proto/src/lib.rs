/*
 * Copyright (C) 2026 The Android Open Source Project
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

//! Crate containing protos used in aflags
mod auto_generated {
    pub use aflags_rust_proto::aflags::Flag as ProtoFlag;
    pub use aflags_rust_proto::aflags::FlagList as ProtoFlagList;
    pub use aflags_rust_proto::aflags::FlagPermission as ProtoFlagPermission;
    pub use aflags_rust_proto::aflags::FlagStorageBackend as ProtoFlagStorageBackend;
    pub use aflags_rust_proto::aflags::ValuePickedFrom as ProtoValuePickedFrom;
}

pub use auto_generated::*;
