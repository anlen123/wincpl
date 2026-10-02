package com.jianzang.phone

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VerificationFilterTest {
    @Test fun acceptsChineseKeywords() {
        for (word in listOf("验证码", "校验码", "动态码", "确认码", "口令")) {
            assertTrue(word, VerificationFilter.matches("您的${word}是 123456"))
        }
    }

    @Test fun acceptsEnglishKeywordsIgnoringCase() {
        assertTrue(VerificationFilter.matches("Your CODE is 123456"))
        assertTrue(VerificationFilter.matches("Your Pin is 1234"))
    }

    @Test fun ignoresOrdinaryMessagesAndBareNumbers() {
        assertFalse(VerificationFilter.matches("今晚一起吃饭"))
        assertFalse(VerificationFilter.matches("123456"))
        assertFalse(VerificationFilter.matches(""))
    }

    @Test fun matchesKeywordAcrossMergedSmsSegments() {
        assertTrue(VerificationFilter.matches(listOf("您的验证", "码是 123456").joinToString("")))
    }
}
