package dev.veloxdesk.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DisplaysJsonParserTest {
    @Test
    fun parseDisplaysJson_supportsSnakeAndCamelCase() {
        val json = """
            [
              {"index":0,"name":"Display 1","x":0,"y":0,"width":2560,"height":1440,"refresh_hz":144,"primary":true},
              {"index":1,"name":"Side Monitor","x":2560,"y":0,"width":1920,"height":1080,"refreshHz":60,"primary":false}
            ]
        """.trimIndent()

        val displays = SessionViewModel.parseDisplaysJson(json)
        assertEquals(2, displays.size)
        assertEquals(0, displays[0].index)
        assertEquals("Display 1", displays[0].name)
        assertEquals(2560, displays[0].width)
        assertEquals(144, displays[0].refreshHz)
        assertTrue(displays[0].primary)

        assertEquals(1, displays[1].index)
        assertEquals("Side Monitor", displays[1].name)
        assertEquals(60, displays[1].refreshHz)
        assertFalse(displays[1].primary)
    }

    @Test
    fun parseDisplaysJson_handlesMalformedAndEmpty() {
        assertTrue(SessionViewModel.parseDisplaysJson("").isEmpty())
        assertTrue(SessionViewModel.parseDisplaysJson("{invalid").isEmpty())
    }
}
