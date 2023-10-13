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

import static androidx.test.platform.app.InstrumentationRegistry.getInstrumentation;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.assertEquals;
import static org.junit.Assume.assumeTrue;
import static org.mockito.Mockito.mock;
import static org.mockito.Mockito.any;
import static org.mockito.Mockito.anyInt;
import static org.mockito.Mockito.anyLong;
import android.content.Context;
import java.util.HashMap;

import android.content.ContextWrapper;
import org.mockito.Mockito;
import android.provider.DeviceConfig;
import android.provider.DeviceConfig.Properties;

import androidx.test.platform.app.InstrumentationRegistry;
import androidx.test.runner.AndroidJUnit4;
import android.app.AlarmManager;

import java.io.IOException;

import org.junit.Test;
import org.junit.Before;
import org.junit.runner.RunWith;

@RunWith(AndroidJUnit4.class)
public class BootNotificationCreatorTest {
    Context mockContext;
    AlarmManager mockAlarmManager;

    BootNotificationCreator bootNotificationCreator;

    @Before
    public void setUp() {
        mockAlarmManager = mock(AlarmManager.class);
        mockContext = new ContextWrapper(getInstrumentation().getTargetContext()) {
            @Override
            public Object getSystemService(String name) {
                if (name.equals(Context.ALARM_SERVICE)) {
                    return mockAlarmManager;
                } else {
                   return super.getSystemService(name);
                }
            }
        };
        bootNotificationCreator = new BootNotificationCreator(mockContext);
    }

    @Test
    public void testNotificationScheduledWhenFlagStaged() {
        HashMap<String, String> flags = new HashMap();
        flags.put("test", "flag");
        Properties properties = new Properties("staged", flags);

        bootNotificationCreator.onPropertiesChanged(properties);

        Mockito.verify(mockAlarmManager).setExact(anyInt(), anyLong(), any());
    }
}
