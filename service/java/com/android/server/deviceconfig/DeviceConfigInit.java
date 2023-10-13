package com.android.server.deviceconfig;

import java.io.FileDescriptor;
import java.io.IOException;

import android.content.Intent;
import android.annotation.NonNull;
import android.annotation.SystemApi;
import android.content.Context;
import android.os.AsyncTask;
import android.os.Binder;
import android.provider.DeviceConfig;
import android.provider.DeviceConfigManager;
import android.provider.UpdatableDeviceConfigServiceReadiness;
import android.content.ServiceConnection;
import android.os.IBinder;
import android.content.ComponentName;

import android.provider.aidl.IDeviceConfigManager;
import android.util.Slog;

import com.android.modules.utils.build.SdkLevel;

import com.android.server.SystemService;

import static com.android.server.deviceconfig.Flags.enableRebootNotification;

/** @hide */
@SystemApi(client = SystemApi.Client.SYSTEM_SERVER)
public class DeviceConfigInit {
    private static final String TAG = "DEVICE_CONFIG_INIT";
    private static final String STAGED_NAMESPACE = "staged";

    private DeviceConfigInit() {
        // do not instantiate
    }

    /** @hide */
    @SystemApi(client = SystemApi.Client.SYSTEM_SERVER)
    public static class Lifecycle extends SystemService {
        private DeviceConfigServiceImpl mService;

        /** @hide */
        @SystemApi(client = SystemApi.Client.SYSTEM_SERVER)
        public Lifecycle(@NonNull Context context) {
            super(context);
            // this service is always instantiated but should only launch subsequent service(s)
            // if the module is ready
            if (UpdatableDeviceConfigServiceReadiness.shouldStartUpdatableService()) {
                mService = new DeviceConfigServiceImpl(getContext());
                publishBinderService(DeviceConfig.SERVICE_NAME, mService);
            }
            applyBootstrapValues();
        }

        /**
         * @hide
         */
        @Override
        public void onStart() {
            if (enableRebootNotification()) {
                DeviceConfig.addOnPropertiesChangedListener(
                    STAGED_NAMESPACE,
                    AsyncTask.THREAD_POOL_EXECUTOR,
                    new BootNotificationCreator(getContext().getApplicationContext()));
            }
        }

        private void applyBootstrapValues() {
            if (SdkLevel.isAtLeastV()) {
                try {
                    new DeviceConfigBootstrapValues().applyValuesIfNeeded();
                } catch (RuntimeException e) {
                    Slog.e(TAG, "Failed to load boot overrides", e);
                    throw e;
                } catch (IOException e) {
                    Slog.e(TAG, "Failed to load boot overrides", e);
                    throw new RuntimeException(e);
                }
            }
        }
    }

}
