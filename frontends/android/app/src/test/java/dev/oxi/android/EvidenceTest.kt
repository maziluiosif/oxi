package dev.oxi.android

import org.junit.Assert.assertEquals
import org.junit.Test

class EvidenceTest {
    private fun check(id: String, exit: Int?) = EvidenceItem(id, "check", "check", id, "oxi", false, null, "cargo test", exit, false, "", 10)
    private fun capture(id: String, origin: String) = EvidenceItem(id, "image", "after", id, origin, false, "/tmp/$id.png", null, null, false, "", 0)

    private fun report(vararg items: EvidenceItem, stale: Boolean = false) =
        Evidence(stale, requested = true, submitted = true, summary = "", items = items.toList(), claims = emptyList())

    @Test
    fun claimsFollowTheDesktopRules() {
        val r = report(check("ok", 0), check("bad", 1), capture("shot", "oxi"), capture("own", "agent"))
        assertEquals(ClaimStatus.Attested, r.status(Claim("tests pass", listOf("ok", "shot"))))
        assertEquals(ClaimStatus.Contradicted, r.status(Claim("fixed", listOf("ok", "bad"))))
        // Uncited, unknown ids, and agent files oxi can't vouch for.
        assertEquals(ClaimStatus.Declared, r.status(Claim("works", emptyList())))
        assertEquals(ClaimStatus.Declared, r.status(Claim("works", listOf("ok", "missing"))))
        assertEquals(ClaimStatus.Declared, r.status(Claim("looks right", listOf("own"))))
    }

    @Test
    fun staleProofOnlyDeclares() {
        val r = report(check("ok", 0), check("bad", 1), stale = true)
        assertEquals(ClaimStatus.Declared, r.status(Claim("tests pass", listOf("ok"))))
        assertEquals(ClaimStatus.Contradicted, r.status(Claim("fixed", listOf("bad"))))
    }
}
