package android.provider;

import java.util.Arrays;
import java.util.List;

/**
 * Contains the list of flags that can be written with ALLOWLISTED_WRITE_DEVICE_CONFIG.
 * <p>
 * A security review is required for any flag that's added to this list. To add to the
 * list, create a change and tag the OWNER. In the change description, include a
 * description of the flag's functionality, and a justification for why it needs to be
 * allowlisted.
 */
final class WritableFlags {
    public static final List<String> ALLOWLIST =
            Arrays.asList(
                "adservices/disable_sdk_sandbox",
                "adservices/enforce_broadcast_receiver_restrictions",
                "adservices/fledge_ad_selection_enforce_foreground_status_custom_audience",
                "adservices/fledge_custom_audience_max_count",
                "adservices/fledge_custom_audience_max_num_ads",
                "adservices/fledge_custom_audience_max_owner_count",
                "adservices/fledge_custom_audience_per_app_max_count",
                "adservices/fledge_js_isolate_enforce_max_heap_size",
                "adservices/fledge_js_isolate_max_heap_size_bytes",
                "adservices/sdk_request_permits_per_second",
                "adservices/sdksandbox_customized_sdk_context_enabled",
                "alarm_manager/allow_while_idle_compat_quota",
                "autofill/smart_suggestion_supported_modes",
                "battery_saver/location_mode",
                "configuration/namespace_to_package_mapping",
                "constrain_display_apis/always_constrain_display_apis",
                "constrain_display_apis/never_constrain_display_apis",
                "constrain_display_apis/never_constrain_display_apis_all_packages",
                "device_policy_manager/disable_resources_updatability",
                "flipendo/default_savings_mode_launch",
                "flipendo/essential_apps",
                "flipendo/flipendo_enabled_launch",
                "flipendo/grayscale_enabled_launch",
                "flipendo/lever_ble_scanning_enabled_launch",
                "flipendo/lever_hotspot_enabled_launch",
                "flipendo/lever_work_profile_enabled_launch",
                "flipendo/resuspend_delay_minutes",
                "jobscheduler/fc_enable_flexibility",
                "location/ignore_settings_allowlist",
                "low_power_standby/enable_policy",
                "namespace1/key1",
                "namespace1/key2",
                "namespace2/key1",
                "namespace2/key2",
                "namespace/key",
                "package_manager_service/incfs_default_timeouts",
                "package_manager_service/known_digesters_list",
                "privacy/permissions_hub_enabled",
                "privacy/safety_center_is_enabled",
                "rollback/enable_rollback_timeout",
                "rollback/watchdog_explicit_health_check_enabled",
                "rollback/watchdog_request_timeout_millis",
                "rollback/watchdog_trigger_failure_count",
                "rollback/watchdog_trigger_failure_duration_millis",
                "rollback_boot/rollback_lifetime_in_millis",
                "systemui/apply_sharing_app_limits_in_sysui",
                "systemui/nas_generate_actions",
                "systemui/nas_generate_replies",
                "systemui/nas_max_messages_to_extract",
                "systemui/nas_max_suggestions",
                "testspace/another",
                "testspace/flagname",
                "textclassifier/config_updater_model_enabled",
                "textclassifier/key",
                "textclassifier/key2",
                "textclassifier/manifest_url_annotator_en",
                "textclassifier/manifest_url_annotator_ru",
                "textclassifier/model_download_backoff_delay_in_millis",
                "textclassifier/model_download_manager_enabled",
                "textclassifier/multi_language_support_enabled",
                "textclassifier/testing_locale_list_override",
                "textclassifier/textclassifier_service_package_override",
                "window_manager/enable_default_rescind_bal_privileges_from_pending_intent_sender",
                "wrong/nas_generate_replies"
            );
}
