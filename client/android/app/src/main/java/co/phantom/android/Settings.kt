package co.phantom.android

import android.content.Context

/*
 * Preferences and the enumerations the UI binds to.
 *
 * Key names are identical to the HarmonyOS client's `phantom_ui` preferences
 * on purpose: the two clients answer the same questions, and keeping the
 * vocabulary aligned is what makes "the setting exists on one but not the
 * other" a bug you can spot by reading either file.
 */

/** Routing mode, matching the `mode` argument of `startTunnelWithURI`. */
enum class ProxyMode(val key: String, val label: String, val hint: String) {
    PROXY("proxy", "全局", "所有流量走服务器"),
    SMART("smart", "智能", "仅白名单域名走服务器"),
    DIRECT("direct", "直连", "全部直连（相当于关闭分流）");

    companion object {
        fun from(key: String): ProxyMode = entries.firstOrNull { it.key == key } ?: SMART
    }
}

/**三态主题：跟随系统是默认，另外两档用于强制。 */
enum class ThemeMode(val key: String, val label: String) {
    SYSTEM("system", "跟随系统"),
    LIGHT("light", "浅色"),
    DARK("dark", "深色");

    companion object {
        fun from(key: String): ThemeMode = entries.firstOrNull { it.key == key } ?: SYSTEM
    }
}

/**
 * The app's persisted settings.
 *
 * Deliberately a thin wrapper over `SharedPreferences` rather than DataStore:
 * everything here is written from the UI thread on user action, read once at
 * startup, and small enough that a synchronous read costs nothing.
 */
class Prefs(context: Context) {
    private val prefs = context.applicationContext
        .getSharedPreferences("phantom_ui", Context.MODE_PRIVATE)

    var serverUri: String
        get() = prefs.getString(KEY_URI, "") ?: ""
        set(value) = prefs.edit().putString(KEY_URI, value).apply()

    var proxyMode: ProxyMode
        get() = ProxyMode.from(prefs.getString(KEY_MODE, ProxyMode.SMART.key) ?: "")
        set(value) = prefs.edit().putString(KEY_MODE, value.key).apply()

    var themeMode: ThemeMode
        get() = ThemeMode.from(prefs.getString(KEY_THEME, ThemeMode.SYSTEM.key) ?: "")
        set(value) = prefs.edit().putString(KEY_THEME, value.key).apply()

    /** Direct-routed traffic is the noisy majority; hidden unless asked for. */
    var showDirectLogs: Boolean
        get() = prefs.getBoolean(KEY_SHOW_DIRECT, false)
        set(value) = prefs.edit().putBoolean(KEY_SHOW_DIRECT, value).apply()

    var tunTrace: Boolean
        get() = prefs.getBoolean(KEY_TUN_TRACE, false)
        set(value) = prefs.edit().putBoolean(KEY_TUN_TRACE, value).apply()

    /**
     * User whitelist rules, one `kind:value` per line (see [RuleFormat]).
     *
     * One string rather than a `StringSet`: order matters to the editor, and a
     * set silently reorders, which would make entries jump around.
     */
    var userRules: List<String>
        get() = prefs.getString(KEY_USER_RULES, "")?.split('\n').orEmpty().filter { it.isNotBlank() }
        set(value) = prefs.edit().putString(KEY_USER_RULES, value.joinToString("\n")).apply()

    /** Remembered connections, most recent first (see [parseHistory]). */
    var history: List<ServerHistoryEntry>
        get() = parseHistory(prefs.getString(KEY_HISTORY, "") ?: "")
        set(value) = prefs.edit().putString(KEY_HISTORY, serializeHistory(value)).apply()

    private companion object {
        const val KEY_URI = "serverUri"
        const val KEY_MODE = "proxyMode"
        const val KEY_THEME = "themeMode"
        const val KEY_SHOW_DIRECT = "showDirectLogs"
        const val KEY_TUN_TRACE = "tunTrace"
        const val KEY_HISTORY = "serverHistory"
        const val KEY_USER_RULES = "userRules"
    }
}
