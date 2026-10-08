package com.terrakit.core

/**
 * A minimal JSON reader.
 *
 * Hand-rolled rather than taken from a library, because [ADR-007](../docs/adr/ADR-007-language-stack.md)'s
 * dependency rule covers the SDK and adding a parser for a manifest would be a poor trade. It reads
 * the whole of JSON — objects, arrays, strings with escapes, numbers, `true`/`false`/`null` — so it
 * is not tied to the manifest's shape.
 *
 * Used in two places: the runtime, to read `manifest.json`, and the JVM test suites, to read the
 * shared battery and the recorded expectations. Both need to fail loudly rather than guess, so every
 * malformed input raises [JsonException] with the offset.
 *
 * This is a reader, not a writer. Nothing in the SDK serializes JSON: `--json` is the CLI's job.
 */
internal object Json {

    fun parse(text: String): Any? = Parser(text).parseDocument()

    class JsonException(message: String) : IllegalArgumentException(message)

    private class Parser(private val text: String) {
        private var index = 0

        fun parseDocument(): Any? {
            skipWhitespace()
            val value = parseValue()
            skipWhitespace()
            if (index != text.length) fail("trailing content after the top-level value")
            return value
        }

        private fun parseValue(): Any? {
            skipWhitespace()
            if (index >= text.length) fail("unexpected end of input")
            return when (val c = text[index]) {
                '{' -> parseObject()
                '[' -> parseArray()
                '"' -> parseString()
                't' -> parseLiteral("true", true)
                'f' -> parseLiteral("false", false)
                'n' -> parseLiteral("null", null)
                else -> if (c == '-' || c.isDigit()) parseNumber() else fail("unexpected '$c'")
            }
        }

        private fun parseObject(): Map<String, Any?> {
            expect('{')
            val result = LinkedHashMap<String, Any?>()
            skipWhitespace()
            if (peek() == '}') { index++; return result }
            while (true) {
                skipWhitespace()
                if (peek() != '"') fail("expected a key")
                val key = parseString()
                skipWhitespace()
                expect(':')
                result[key] = parseValue()
                skipWhitespace()
                when (peek()) {
                    ',' -> index++
                    '}' -> { index++; return result }
                    else -> fail("expected ',' or '}'")
                }
            }
        }

        private fun parseArray(): List<Any?> {
            expect('[')
            val result = mutableListOf<Any?>()
            skipWhitespace()
            if (peek() == ']') { index++; return result }
            while (true) {
                result += parseValue()
                skipWhitespace()
                when (peek()) {
                    ',' -> index++
                    ']' -> { index++; return result }
                    else -> fail("expected ',' or ']'")
                }
            }
        }

        private fun parseString(): String {
            expect('"')
            val out = StringBuilder()
            while (true) {
                if (index >= text.length) fail("unterminated string")
                when (val c = text[index]) {
                    '"' -> { index++; return out.toString() }
                    '\\' -> {
                        index++
                        if (index >= text.length) fail("unterminated escape")
                        when (val escape = text[index]) {
                            '"' -> out.append('"')
                            '\\' -> out.append('\\')
                            '/' -> out.append('/')
                            'b' -> out.append('\b')
                            'f' -> out.append('')
                            'n' -> out.append('\n')
                            'r' -> out.append('\r')
                            't' -> out.append('\t')
                            'u' -> {
                                if (index + 4 >= text.length) fail("truncated \\u escape")
                                val hex = text.substring(index + 1, index + 5)
                                out.append(
                                    // Surrogate halves are kept as-is; a pair appended in sequence
                                    // forms the astral character when the String is built, so no
                                    // pairing logic is needed here.
                                    hex.toInt(16).toChar()
                                )
                                index += 4
                            }
                            else -> fail("unhandled escape '\\$escape'")
                        }
                        index++
                    }
                    else -> {
                        out.append(c)
                        index++
                    }
                }
            }
        }

        private fun parseNumber(): Any {
            val start = index
            if (peek() == '-') index++
            while (index < text.length && (text[index].isDigit() || text[index] in ".eE+-")) index++
            val raw = text.substring(start, index)
            return raw.toLongOrNull() ?: raw.toDoubleOrNull() ?: fail("'$raw' is not a number")
        }

        private fun <T> parseLiteral(literal: String, value: T): T {
            if (!text.startsWith(literal, index)) fail("expected '$literal'")
            index += literal.length
            return value
        }

        private fun peek(): Char? = if (index < text.length) text[index] else null

        private fun expect(c: Char) {
            if (peek() != c) fail("expected '$c'")
            index++
        }

        private fun skipWhitespace() {
            while (index < text.length && text[index].isWhitespace()) index++
        }

        private fun fail(message: String): Nothing =
            throw JsonException("invalid JSON at offset $index: $message")
    }
}

/** Read a field from a parsed object, or `null` when it is absent or not this type. */
internal fun Any?.jsonObject(): Map<String, Any?>? = this as? Map<String, Any?>

/** Read a field from a parsed object, or `null`. */
internal fun Map<String, Any?>.string(key: String): String? = this[key] as? String

/** Read an integer field from a parsed object. JSON has one number type, so `Double` counts. */
internal fun Map<String, Any?>.int(key: String): Int? = (this[key] as? Number)?.toInt()

/** Read a double field from a parsed object. */
internal fun Map<String, Any?>.double(key: String): Double? = (this[key] as? Number)?.toDouble()

/** Read a list of strings from a parsed object. */
internal fun Map<String, Any?>.stringList(key: String): List<String> =
    (this[key] as? List<*>)?.mapNotNull { it as? String } ?: emptyList()