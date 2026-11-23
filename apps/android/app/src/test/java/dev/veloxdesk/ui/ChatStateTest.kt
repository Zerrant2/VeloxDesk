package dev.veloxdesk.ui

import dev.veloxdesk.bridge.ChatState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.TimeZone

class ChatStateTest {
    @Test
    fun incomingMessages_incrementUnreadWhenClosed_andClearOnOpen() {
        var state = ChatState()
        assertEquals(0, state.unreadCount)
        assertFalse(state.isOpen)

        state = state.onIncomingReceived(1L, "Desktop", "Hello", 1_700_000_000_000L)
        state = state.onIncomingReceived(2L, "Desktop", "Привет", 1_700_000_060_000L)
        assertEquals(2, state.messages.size)
        assertEquals(2, state.unreadCount)
        assertFalse(state.messages[0].outgoing)
        assertTrue(state.messages[0].delivered)

        // Opening the chat drawer clears the unread badge.
        state = state.setOpen(true)
        assertTrue(state.isOpen)
        assertEquals(0, state.unreadCount)

        // Messages arriving while chat is open do NOT increment unread count.
        state = state.onIncomingReceived(3L, "Desktop", "Still here?", 1_700_000_120_000L)
        assertEquals(3, state.messages.size)
        assertEquals(0, state.unreadCount)

        // Closing and receiving another message resumes unread counting.
        state = state.setOpen(false)
        state = state.onIncomingReceived(4L, "Desktop", "Ping", 1_700_000_180_000L)
        assertEquals(1, state.unreadCount)
    }

    @Test
    fun outgoingMessages_trackDeliveryAckAndIgnoreUnknownAck() {
        var state = ChatState()
        state = state.onOutgoingSent(10L, "Pixel", "First", 1_700_000_000_000L)
        state = state.onOutgoingSent(11L, "Pixel", "Second", 1_700_000_001_000L)
        assertEquals(2, state.messages.size)
        assertEquals(0, state.unreadCount)
        assertTrue(state.messages[0].outgoing)
        assertFalse(state.messages[0].delivered)
        assertFalse(state.messages[1].delivered)

        // Ack for message 10 marks only message 10 as delivered.
        state = state.onAckReceived(10L)
        assertTrue(state.messages[0].delivered)
        assertFalse(state.messages[1].delivered)

        // Unknown/duplicate acks do not crash or falsely mark other messages.
        state = state.onAckReceived(999L)
        state = state.onAckReceived(0L)
        assertTrue(state.messages[0].delivered)
        assertFalse(state.messages[1].delivered)

        state = state.onAckReceived(11L)
        assertTrue(state.messages[1].delivered)
    }

    @Test
    fun preReceivedAck_marksOutgoingMessageDeliveredImmediately() {
        // Simulates Rust background thread delivering onChatAck before caller appends outgoing item.
        var state = ChatState()
        state = state.onAckReceived(42L)
        state = state.onOutgoingSent(42L, "Pixel", "Fast LAN message", 1_700_000_000_000L)
        assertEquals(1, state.messages.size)
        assertTrue(state.messages[0].delivered)
    }

    @Test
    fun ignoresInvalidInput_capsHistory_formatsTimestamp_andClearsCleanly() {
        var state = ChatState()
        state = state.onOutgoingSent(0L, "Pixel", "Zero ID ignored", 1_000L)
        state = state.onOutgoingSent(1L, "Pixel", "", 1_000L)
        state = state.onIncomingReceived(1L, "Host", "", 1_000L)
        assertTrue(state.messages.isEmpty())

        for (i in 1L..(ChatState.MAX_CHAT_MESSAGES + 25L)) {
            state = state.onIncomingReceived(i, "Host", "Msg $i", 1_700_000_000_000L + i)
        }
        assertEquals(ChatState.MAX_CHAT_MESSAGES, state.messages.size)
        assertEquals(26L, state.messages.first().id)

        // 2023-11-14T22:13:20Z -> 22:13 in UTC
        assertEquals("22:13", ChatState.formatTimestamp(1_700_000_000_000L, TimeZone.getTimeZone("UTC")))
        assertEquals("", ChatState.formatTimestamp(0L, TimeZone.getTimeZone("UTC")))

        state = state.clear()
        assertTrue(state.messages.isEmpty())
        assertEquals(0, state.unreadCount)
        assertFalse(state.isOpen)
    }
}
