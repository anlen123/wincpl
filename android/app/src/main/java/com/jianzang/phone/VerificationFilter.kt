package com.jianzang.phone

/** 只判断关键词，不保存短信正文；普通短信只产生匿名的忽略原因日志。 */
object VerificationFilter {
    private val keywords = listOf("验证码", "校验码", "动态码", "确认码", "口令", "code", "pin")

    fun matches(text: String): Boolean = keywords.any { text.contains(it, ignoreCase = true) }
}
