package com.jianzang.phone

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** 电脑端生成的一条配对信息。 */
data class Pairing(
    val host: String,
    val port: Int,
    val token: String,
    val name: String,
) {
    val link: String
        get() = "jianzang://pair?host=$host&port=$port&token=$token&name=${Uri.encode(name)}"

    val address: String
        get() = "$host:$port"
}

/**
 * 手机端所有持久化都在这里：配对信息、转发开关、最近记录。
 * 只用 SharedPreferences，不引入数据库。
 */
object PairingStore {
    private const val PREFS = "jianzang"
    private const val KEY_HOST = "host"
    private const val KEY_PORT = "port"
    private const val KEY_TOKEN = "token"
    private const val KEY_NAME = "name"
    private const val KEY_ENABLED = "enabled"
    private const val KEY_LOG = "log"
    private const val LOG_LIMIT = 100

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun load(context: Context): Pairing? {
        val store = prefs(context)
        val host = store.getString(KEY_HOST, null) ?: return null
        val token = store.getString(KEY_TOKEN, null) ?: return null
        val port = store.getInt(KEY_PORT, 0)
        if (host.isEmpty() || token.isEmpty() || port !in 1..65535) return null
        return Pairing(host, port, token, store.getString(KEY_NAME, null) ?: "剪藏")
    }

    fun save(
        context: Context,
        pairing: Pairing,
    ) {
        prefs(context)
            .edit()
            .putString(KEY_HOST, pairing.host)
            .putInt(KEY_PORT, pairing.port)
            .putString(KEY_TOKEN, pairing.token)
            .putString(KEY_NAME, pairing.name)
            .apply()
    }

    /** 从电脑设置面板里显示的链接解析；也容忍前后混进了别的文字（例如整段聊天记录）。 */
    fun parse(payload: String): Pairing? {
        val text = payload.trim()
        val start = text.indexOf("jianzang://pair")
        if (start < 0) return null
        val uri = runCatching { Uri.parse(text.substring(start)) }.getOrNull() ?: return null
        if (uri.scheme != "jianzang" || uri.host != "pair") return null
        val host = uri.getQueryParameter("host")?.trim().orEmpty()
        val token = uri.getQueryParameter("token")?.trim().orEmpty()
        val port = uri.getQueryParameter("port")?.trim()?.toIntOrNull() ?: return null
        if (host.isEmpty() || token.isEmpty() || port !in 1..65535) return null
        val name = uri.getQueryParameter("name")?.takeIf { it.isNotBlank() } ?: "剪藏"
        return Pairing(host, port, token, name)
    }

    fun isEnabled(context: Context): Boolean = prefs(context).getBoolean(KEY_ENABLED, false)

    fun setEnabled(
        context: Context,
        enabled: Boolean,
    ) {
        prefs(context).edit().putBoolean(KEY_ENABLED, enabled).apply()
    }

    fun log(context: Context): List<String> = prefs(context).getString(KEY_LOG, "")?.split('\n')?.filter { it.isNotBlank() } ?: emptyList()

    // 广播线程和界面线程可能同时追加；串行读改写，避免丢失诊断记录。
    @Synchronized
    fun appendLog(
        context: Context,
        line: String,
    ) {
        val stamp = SimpleDateFormat("MM-dd HH:mm:ss", Locale.CHINA).format(Date())
        val lines = (listOf("$stamp ${line.replace('\n', ' ').replace('\r', ' ')}") + log(context)).take(LOG_LIMIT)
        prefs(context).edit().putString(KEY_LOG, lines.joinToString("\n")).apply()
    }

    fun observe(
        context: Context,
        listener: SharedPreferences.OnSharedPreferenceChangeListener,
    ) {
        prefs(context).registerOnSharedPreferenceChangeListener(listener)
    }

    fun unobserve(
        context: Context,
        listener: SharedPreferences.OnSharedPreferenceChangeListener,
    ) {
        prefs(context).unregisterOnSharedPreferenceChangeListener(listener)
    }
}
