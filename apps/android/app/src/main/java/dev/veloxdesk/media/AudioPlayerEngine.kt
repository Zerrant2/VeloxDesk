package dev.veloxdesk.media

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.util.Log

/**
 * Thread-safe, non-blocking low-latency [AudioTrack] engine for streaming decoded PCM16 audio
 * from the native Rust audio thread.
 */
class AudioPlayerEngine {
    private val lock = Any()

    private var track: AudioTrack? = null
    private var currentSampleRate: Int = 0
    private var currentChannels: Int = 0
    private var muted: Boolean = false
    private var released: Boolean = false

    fun configure(sampleRate: Int, channels: Int) {
        synchronized(lock) {
            if (released) return
            if (sampleRate <= 0 || channels <= 0) return
            configureLocked(sampleRate, channels)
        }
    }

    fun writePcm(sampleRate: Int, channels: Int, pcm16: ShortArray) {
        if (pcm16.isEmpty() || sampleRate <= 0 || channels <= 0) return
        synchronized(lock) {
            if (released || muted) return
            val clampedChannels = if (channels <= 1) 1 else 2

            if (track == null || currentSampleRate != sampleRate || currentChannels != clampedChannels) {
                if (!configureLocked(sampleRate, clampedChannels)) {
                    return
                }
            }

            val activeTrack = track ?: return
            try {
                if (activeTrack.playState != AudioTrack.PLAYSTATE_PLAYING) {
                    activeTrack.play()
                }
                val written = activeTrack.write(pcm16, 0, pcm16.size, AudioTrack.WRITE_NON_BLOCKING)
                if (written == AudioTrack.ERROR_DEAD_OBJECT || written == AudioTrack.ERROR_INVALID_OPERATION) {
                    Log.w(TAG, "AudioTrack write error=$written; resetting track")
                    releaseTrackLocked()
                }
            } catch (t: Throwable) {
                Log.w(TAG, "AudioTrack write failed: ${t.message}")
                releaseTrackLocked()
            }
        }
    }

    fun setMuted(muted: Boolean) {
        synchronized(lock) {
            this.muted = muted
            val activeTrack = track ?: return
            try {
                if (muted) {
                    if (activeTrack.playState == AudioTrack.PLAYSTATE_PLAYING) {
                        activeTrack.pause()
                    }
                    activeTrack.flush()
                } else if (!released) {
                    if (activeTrack.playState != AudioTrack.PLAYSTATE_PLAYING) {
                        activeTrack.play()
                    }
                }
            } catch (t: Throwable) {
                Log.w(TAG, "AudioTrack mute toggle failed: ${t.message}")
                releaseTrackLocked()
            }
        }
    }

    /**
     * Stops and releases the active [AudioTrack] between sessions while keeping this engine
     * reusable for subsequent [configure] / [writePcm] calls.
     */
    fun stop() {
        synchronized(lock) {
            if (released) return
            releaseTrackLocked()
        }
    }

    fun release() {
        synchronized(lock) {
            released = true
            releaseTrackLocked()
        }
    }

    private fun configureLocked(sampleRate: Int, channels: Int): Boolean {
        val clampedChannels = if (channels <= 1) 1 else 2
        if (track != null && currentSampleRate == sampleRate && currentChannels == clampedChannels) {
            return true
        }

        releaseTrackLocked()

        val channelMask = if (clampedChannels == 1) {
            AudioFormat.CHANNEL_OUT_MONO
        } else {
            AudioFormat.CHANNEL_OUT_STEREO
        }

        val minBufBytes = try {
            AudioTrack.getMinBufferSize(sampleRate, channelMask, AudioFormat.ENCODING_PCM_16BIT)
        } catch (_: Throwable) {
            0
        }
        val frameSizeBytes = clampedChannels * BYTES_PER_SAMPLE
        val target20msBytes = (sampleRate * frameSizeBytes * TARGET_BUFFER_MS) / 1000
        val rawBufBytes = maxOf(minBufBytes.coerceAtLeast(0), target20msBytes, MIN_BUFFER_BYTES)
        val alignedBufBytes = ((rawBufBytes + frameSizeBytes - 1) / frameSizeBytes) * frameSizeBytes

        val created = buildAudioTrack(sampleRate, channelMask, alignedBufBytes, lowLatency = true)
            ?: buildAudioTrack(sampleRate, channelMask, alignedBufBytes, lowLatency = false)
            ?: return false

        track = created
        currentSampleRate = sampleRate
        currentChannels = clampedChannels

        if (!muted) {
            try {
                created.play()
            } catch (t: Throwable) {
                Log.w(TAG, "AudioTrack.play() failed: ${t.message}")
                releaseTrackLocked()
                return false
            }
        }
        return true
    }

    private fun buildAudioTrack(
        sampleRate: Int,
        channelMask: Int,
        bufferSizeBytes: Int,
        lowLatency: Boolean,
    ): AudioTrack? {
        return try {
            val attrs = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_GAME)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            val format = AudioFormat.Builder()
                .setSampleRate(sampleRate)
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setChannelMask(channelMask)
                .build()

            val builder = AudioTrack.Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(format)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(bufferSizeBytes)
                .setPerformanceMode(
                    if (lowLatency) AudioTrack.PERFORMANCE_MODE_LOW_LATENCY else AudioTrack.PERFORMANCE_MODE_NONE,
                )

            val built = builder.build()
            if (built.state != AudioTrack.STATE_INITIALIZED) {
                built.release()
                null
            } else {
                built
            }
        } catch (t: Throwable) {
            Log.w(TAG, "AudioTrack build failed (lowLatency=$lowLatency): ${t.message}")
            null
        }
    }

    private fun releaseTrackLocked() {
        val t = track
        track = null
        currentSampleRate = 0
        currentChannels = 0
        if (t != null) {
            try {
                if (t.playState == AudioTrack.PLAYSTATE_PLAYING) {
                    t.pause()
                    t.flush()
                }
                t.stop()
            } catch (_: Throwable) {
            }
            try {
                t.release()
            } catch (_: Throwable) {
            }
        }
    }

    companion object {
        private const val TAG = "AudioPlayerEngine"
        private const val BYTES_PER_SAMPLE = 2
        private const val TARGET_BUFFER_MS = 20
        private const val MIN_BUFFER_BYTES = 1024
    }
}
