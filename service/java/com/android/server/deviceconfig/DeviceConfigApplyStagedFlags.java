package com.android.server.deviceconfig;

import android.annotation.SystemApi;
import android.provider.DeviceConfig;
import android.util.Slog;

import java.util.Map;

/**
 * Applies all reboot-staged flags.
 *
 * Currently, flags are staged by being stored in a special DeviceConfig namespace.
 */
class DeviceConfigApplyStagedFlags {
    private static final String TAG = "DeviceConfigApplyStagedFlags";
    private static final String ESCAPE_SLASHES = "\\";

    private DeviceConfigApplyStagedFlags() {}

    /**
     * Applies all flags that were staged for reboot.
     *
     * Flags are staged by being stored in the DeviceConfig namespace "staged".
     * They're stored there with the name {@code namespace*flagName}.
     *
     */
    public static void applyStagedFlags() {
        DeviceConfig.Properties stagedProperties =
            DeviceConfig.getProperties(DeviceConfig.NAMESPACE_REBOOT_STAGING);

        for (Map.Entry<String, String> entry : stagedProperties.getPropertyValues().entrySet()) {
            String name = entry.getKey();
            String value = entry.getValue();

            String delimiter = ESCAPE_SLASHES + DeviceConfig.NAMESPACE_REBOOT_STAGING_DELIMITER;
            String[] parts = name.split(delimiter);
            if (parts.length != 2) {
                Slog.w(TAG, "staged flag name '"
                    + name
                    + "' didn't follow 'namespace*flag' format, not applying");
                continue;
            }
            String namespace = parts[0];
            String flagName = parts[1];

            if (DeviceConfig.setProperty(namespace, flagName, value, true)) {
                if (!DeviceConfig.deleteProperty(
                    DeviceConfig.NAMESPACE_REBOOT_STAGING, name)) {
                    Slog.w(TAG, "failed to delete staged flag '"
                        + namespace + "/" + flagName + ":" + value + "'");
                }
            } else {
                Slog.w(TAG, "failed to apply staged flag '"
                    + namespace + "/" + flagName + ":" + value + "'");
            }
        }
    }
}
