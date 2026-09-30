package com.jianzang.phone

import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.nio.charset.StandardCharsets

/** 电脑端接口的一次应答。 */
sealed interface Reply {
    data class Ok(val code: String?) : Reply
    data class Failed(val message: String) : Reply
}

/**
 * 与电脑上的剪藏通信：局域网明文 HTTP，靠配对令牌鉴权。
 * 只用 HttpURLConnection + org.json，不额外引依赖。
 */
object Relay {
    private const val TIMEOUT_MS = 8000
    private const val TOKEN_HEADER = "X-Jianzang-Token"

    fun ping(pairing: Pairing): Reply = request(pairing, "GET", "/api/v1/ping", null)

    fun sendSms(pairing: Pairing, text: String, from: String): Reply {
        val body = JSONObject()
            .put("text", text)
            .put("from", from)
            .toString()
        return request(pairing, "POST", "/api/v1/sms", body)
    }

    private fun request(pairing: Pairing, method: String, path: String, body: String?): Reply {
        var connection: HttpURLConnection? = null
        return try {
            val url = URL("http://${pairing.host}:${pairing.port}$path")
            connection = (url.openConnection() as HttpURLConnection).apply {
                requestMethod = method
                connectTimeout = TIMEOUT_MS
                readTimeout = TIMEOUT_MS
                setRequestProperty(TOKEN_HEADER, pairing.token)
                setRequestProperty("Accept", "application/json")
                doInput = true
                if (body != null) {
                    doOutput = true
                    setRequestProperty("Content-Type", "application/json; charset=utf-8")
                }
            }
            if (body != null) {
                connection.outputStream.use { it.write(body.toByteArray(StandardCharsets.UTF_8)) }
            }
            val status = connection.responseCode
            val stream = if (status in 200..299) connection.inputStream else connection.errorStream
            val payload = stream?.bufferedReader(StandardCharsets.UTF_8)?.use { it.readText() }.orEmpty()
            if (status in 200..299) Reply.Ok(parseCode(payload)) else Reply.Failed(explain(status, payload))
        } catch (error: Exception) {
            Reply.Failed("连不上电脑（${error.javaClass.simpleName}），检查是否同一 Wi-Fi、剪藏是否在运行")
        } finally {
            connection?.disconnect()
        }
    }

    private fun parseCode(payload: String): String? =
        runCatching { JSONObject(payload).optString("code").takeIf { it.isNotBlank() } }.getOrNull()

    private fun explain(status: Int, payload: String): String {
        val detail = runCatching { JSONObject(payload).optString("error") }.getOrNull().orEmpty()
        val hint = when (status) {
            401 -> "配对令牌不对，请在电脑上重新扫码"
            404 -> "电脑端接口不匹配（版本太旧？）"
            422 -> "这条短信里没找到验证码"
            413 -> "短信太长，被电脑端拒绝"
            503 -> "电脑上还没开启手机接入"
            else -> "电脑端返回 $status"
        }
        return if (detail.isBlank()) hint else "$hint（$detail）"
    }
}
