package dev.oxi.android

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class QuestionTest {
    /** As Claude Code sends `AskUserQuestion` (see `questions()` in src/agent/elicitation.rs). */
    private val ask = JSONObject(
        """
        {"mode": "form", "message": "Which language?", "requestedSchema": {"type": "object",
          "required": ["question_2"],
          "properties": {
            "question_10": {"type": "string", "title": "Third", "oneOf": [{"const": "a", "title": "A"}]},
            "question_2": {"type": "array", "title": "Second", "items": {"anyOf": [
              {"const": "Rust", "title": "Rust", "description": "Fast"}, {"const": "Go", "title": "Go"}]}},
            "question_0": {"type": "string", "title": "First", "enum": ["x", "y"], "enumNames": ["X", "Y"], "default": "y"},
            "question_0_custom": {"type": "string", "title": "Other"},
            "count": {"type": "integer", "title": "Count"},
            "sure": {"type": "boolean", "title": "Sure", "default": true}
          }}}
        """
    )

    @Test
    fun fieldsComeInNaturalOrderWithTheirKinds() {
        val q = parseQuestion(ask)
        assertEquals("Which language?", q.message)
        assertEquals(listOf("count", "question_0", "question_0_custom", "question_2", "question_10", "sure"), q.fields.map { it.key })
        val byKey = q.fields.associateBy { it.key }
        assertEquals(FieldKind.Choice, byKey.getValue("question_0").kind)
        assertEquals(listOf("X", "Y"), byKey.getValue("question_0").options.map { it.label })
        assertEquals(FieldKind.Text, byKey.getValue("question_0_custom").kind)
        assertEquals(FieldKind.Multi, byKey.getValue("question_2").kind)
        assertEquals("Fast", byKey.getValue("question_2").options[0].description)
        assertTrue(byKey.getValue("question_2").required)
        assertEquals(FieldKind.Integer, byKey.getValue("count").kind)
        assertEquals(mapOf("question_0" to "y", "sure" to true), q.defaults())
    }

    @Test
    fun answersBecomeTheFormContent() {
        val q = parseQuestion(ask)
        assertEquals("Second is required", q.content(q.defaults()).exceptionOrNull()?.message)
        val answers = q.defaults() + mapOf("question_2" to setOf("Go", "Rust"), "count" to " 3 ", "question_0_custom" to " ")
        val content = q.content(answers).getOrThrow()
        assertEquals("y", content.getString("question_0"))
        // Picks keep the order of the options, and blank text is no answer.
        assertEquals("""["Rust","Go"]""", content.getJSONArray("question_2").toString())
        assertEquals(3, content.getInt("count"))
        assertEquals(true, content.getBoolean("sure"))
        assertTrue(!content.has("question_0_custom"))
        assertEquals("Count must be a whole number", q.content(answers + ("count" to "1.5")).exceptionOrNull()?.message)
        assertEquals("Count must be a number", q.content(answers + ("count" to "many")).exceptionOrNull()?.message)
    }

    @Test
    fun aPageToVisitHasNoFields() {
        val q = parseQuestion(JSONObject("""{"mode": "url", "message": "Sign in", "url": "https://example.com/login"}"""))
        assertEquals("https://example.com/login", q.url)
        assertTrue(q.fields.isEmpty())
        assertEquals("{}", q.content(emptyMap()).getOrThrow().toString())
    }

    @Test
    fun approvalsCarryTheQuestion() {
        val plain = parseApproval(JSONObject("""{"tool": "bash", "summary": "ls", "allow_prefix": "ls"}"""))!!
        assertNull(plain.question)
        assertEquals("ls", plain.allowPrefix)
        val asked = parseApproval(JSONObject().put("tool", "question").put("summary", "Which?").put("elicitation", ask))!!
        assertEquals(6, asked.question?.fields?.size)
    }

    @Test
    fun digitsCompareAsNumbers() {
        assertTrue(naturalCompare("question_2", "question_10") < 0)
        assertTrue(naturalCompare("question_10", "question_2") > 0)
        assertTrue(naturalCompare("a", "b") < 0)
        assertTrue(naturalCompare("question_1", "question_1_custom") < 0)
        assertEquals(0, naturalCompare("q07", "q7"))
    }

    @Test
    fun theChecklistIsTheLatestOneOfThisTurn() {
        fun tool(name: String, args: String?) = Block.Tool(null, name, "", "", "completed", "", null, args)
        val old = """{"todos":[{"content":"old","status":"completed"}]}"""
        val new = """{"todos":[{"content":"a","status":"completed"},{"content":"b","status":"in_progress"}]}"""
        val messages = listOf(
            Message.User("first", emptyList()),
            Message.Assistant(false, listOf(tool("todo_write", old)), null, null),
            Message.User("second", emptyList()),
            Message.Assistant(true, listOf(tool("TodoWrite", old), Block.Answer("ok"), tool("mcp__oxi__todo_write", new), tool("bash", "{}")), null, null),
        )
        assertEquals(listOf(Todo("a", "completed"), Todo("b", "in_progress")), parseTodos(latestTodoArgs(messages)))
        assertNull(latestTodoArgs(messages + Message.User("third", emptyList())))
        assertNull(parseTodos("""{"todos":[{"content":"cut of"""))
        assertNull(parseTodos("""{"command":"ls"}"""))
    }

    @Test
    fun notificationsQuotePlainText() {
        assertEquals("Done\nFixed the bug in main.rs", snippet("## Done\n\n```rust\n```\n**Fixed** the bug in `main.rs`".replace("\n\n", "\n")))
        assertEquals(11, snippet("a".repeat(50), max = 10).length)
    }

    @Test
    fun gitStateReadsTheServersShape() {
        val g = parseGit(
            JSONObject(
                """{"repo": true, "branch": "dev", "branches": ["dev"], "ahead": 2, "behind": 0,
                "staged": [{"path": "src/a.rs", "status": "M"}], "unstaged": [{"path": "b.txt", "status": "?"}],
                "log": [{"hash": "abc1234", "date": "today", "author": "me", "message": "feat: x"}],
                "diff": {"title": "src/a.rs", "text": "+x"}, "error": null}"""
            )
        )
        assertEquals("dev", g.branch)
        assertEquals(2, g.ahead)
        assertEquals(listOf(GitFile("src/a.rs", "M")), g.staged)
        assertEquals("?", g.unstaged[0].status)
        assertEquals("+x", g.diff?.text)
        assertNull(g.error)
    }
}
