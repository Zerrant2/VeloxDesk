package dev.veloxdesk.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class AuthPasswordTransformTest {
    @Test
    fun uppercaseModeOn_uppercasesAsciiAndStripsWhitespace() {
        assertEquals("A7K9P2", transformPasswordInput("a7k9p2", uppercaseMode = true))
        assertEquals("A7K9P2", transformPasswordInput(" a7k 9p2\t\n", uppercaseMode = true))
        assertEquals("AB-12_CD!", transformPasswordInput("ab-12_cd!", uppercaseMode = true))
        assertEquals("", transformPasswordInput("   ", uppercaseMode = true))
    }

    @Test
    fun uppercaseModeOff_preservesExactMixedCaseAndSpaces() {
        assertEquals("My Secret Pass 123!", transformPasswordInput("My Secret Pass 123!", uppercaseMode = false))
        assertEquals("a7k9p2", transformPasswordInput("a7k9p2", uppercaseMode = false))
        assertEquals("  padded  ", transformPasswordInput("  padded  ", uppercaseMode = false))
    }
}
