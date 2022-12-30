/*
 * Copyright (C) 2023 The Android Open Source Project
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

package com.android.server.deviceconfig;

import android.content.Context;
import android.os.RemoteException;

import com.android.server.deviceconfig.db.DeviceConfigDbAdapter;
import com.android.server.deviceconfig.db.DeviceConfigDbHelper;

import java.util.Map;

/**
 * DeviceConfig Service implementation (updatable via Mainline) that uses a SQLite database as a storage mechanism
 * for the configuration values.
 *
 * @hide
 */
public class DeviceConfigServiceImpl {
    private final DeviceConfigDbAdapter mDbAdapter;

    public DeviceConfigServiceImpl(Context context) {
        DeviceConfigDbHelper dbHelper = new DeviceConfigDbHelper(context);
        mDbAdapter = new DeviceConfigDbAdapter(dbHelper.getWritableDatabase());
    }

    public Map<String, String> getProperties(String namespace, String[] names) throws RemoteException {
        return mDbAdapter.getValuesForNamespace(namespace, names);
    }

    public boolean setProperties(String namespace, Map<String, String> values) {
        return mDbAdapter.setValues(namespace, values);
    }

    public boolean setProperty(String namespace, String key, String value, boolean makeDefault) {
        return mDbAdapter.setValue(namespace, key, value, makeDefault);
    }
}
