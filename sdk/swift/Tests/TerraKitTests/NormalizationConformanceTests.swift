import XCTest

@testable import TerraKit

/// Cross-language normalization conformance.
///
/// `tests/fixtures/normalization.json` is the same oracle the Python pipeline, the Rust CLI and
/// Kotlin are held to. Reading it here is what makes the Swift normalizer *verifiable* rather
/// than merely present: the SDK and the CLI must agree on which places a query matches, and the
/// only way to know they agree is to check them against one set of vectors.
///
/// A failing vector here is a real defect, not a fixture to be adjusted. See
/// `CONTRIBUTING.md` §5.1.
final class NormalizationConformanceTests: XCTestCase {

    private struct Fixture: Decodable {
        let vectors: [Vector]
    }

    private struct Vector: Decodable {
        let n: Int
        let input: String
        let expected: String
        let covers: String?
    }

    /// The fixture lives at the repository root, two levels above this package.
    ///
    /// Resolved from `#filePath` rather than the working directory: SPM does not guarantee a
    /// working directory for tests, and a conformance suite that cannot find its oracle fails in a
    /// way that looks like a pass on CI machines and a failure locally.
    private static var fixtureURL: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // TerraKitTests
            .deletingLastPathComponent()   // Tests
            .deletingLastPathComponent()   // swift
            .deletingLastPathComponent()   // sdk
            .deletingLastPathComponent()   // repository root
            .appendingPathComponent("tests/fixtures/normalization.json")
    }

    private static func loadFixture() throws -> Fixture {
        let url = fixtureURL
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode(Fixture.self, from: data)
    }

    func testEveryFixtureVectorPasses() throws {
        let fixture = try Self.loadFixture()
        var failures: [String] = []

        for vector in fixture.vectors {
            let actual = Normalization.normalize(vector.input)
            if actual != vector.expected {
                failures.append(
                    """
                    #\(vector.n) \(vector.covers ?? "")
                        input:    \(vector.input.debugDescription)
                        expected: \(vector.expected.debugDescription)
                        actual:   \(actual.debugDescription)
                    """
                )
            }
        }

        XCTAssertTrue(
            failures.isEmpty,
            """
            \(failures.count)/\(fixture.vectors.count) vectors failed:

            \(failures.joined(separator: "\n"))

            Do not edit the fixture to make these pass. See CONTRIBUTING.md section 5.1.
            """
        )
    }

    func testOutputIsIdempotentForEveryVector() throws {
        let fixture = try Self.loadFixture()
        for vector in fixture.vectors {
            let once = Normalization.normalize(vector.input)
            XCTAssertEqual(
                Normalization.normalize(once),
                once,
                "vector #\(vector.n) is not idempotent: \(vector.input.debugDescription)"
            )
        }
    }

    func testJapaneseDakutenSurvives() {
        // The defect that made Japanese search return nothing: a categorical mark strip rewrote
        // these into different names and collided カ with ガ.
        for word in ["ガザ", "バ", "パ", "ヴ", "イスタンブール", "東京都"] {
            XCTAssertEqual(Normalization.normalize(word), word)
        }
        XCTAssertNotEqual(Normalization.normalize("カ"), Normalization.normalize("ガ"))
        XCTAssertNotEqual(Normalization.normalize("パ"), Normalization.normalize("ハ"))
        XCTAssertFalse(Normalization.normalize("ガザ").contains(" "))
    }

    func testHangulRecomposes() {
        for word in ["서울", "부산"] {
            XCTAssertEqual(Normalization.normalize(word), word)
        }
    }

    func testSpecExamples() {
        // The examples `core/spec/normalization.md` §8 names explicitly.
        let expected: [(String, String)] = [
            ("Istanbul", "istanbul"),
            ("İstanbul", "istanbul"),
            ("ISTANBUL", "istanbul"),
            ("Atatürk", "ataturk"),
            ("Ataturk", "ataturk"),
            ("Muratpaşa", "muratpasa"),
            ("Türkiye", "turkiye"),
            ("Straße", "strasse"),
            ("Ağrı", "agri"),
            ("İzmir", "izmir"),
        ]
        for (input, want) in expected {
            XCTAssertEqual(Normalization.normalize(input), want, "normalize(\(input.debugDescription))")
        }
    }

    func testApostropheRemovedButHyphenSpaced() {
        // §6: these are different rules and conflating them breaks one or merges two genuinely
        // distinct keys.
        XCTAssertEqual(Normalization.normalize("D'Angelo"), "dangelo")
        XCTAssertEqual(Normalization.normalize("D’Angelo"), "dangelo")
        XCTAssertEqual(Normalization.normalize("Saint-Denis"), "saint denis")
    }

    func testPostalNormalizationIsStricter() {
        XCTAssertEqual(Normalization.postalNormalize("SW1A 1AA"), "sw1a1aa")
        XCTAssertEqual(Normalization.postalNormalize("SW1A1AA"), "sw1a1aa")
        XCTAssertEqual(Normalization.postalNormalize("SW1A-1AA"), "sw1a1aa")
        XCTAssertEqual(Normalization.postalNormalize("07160"), "07160")
        XCTAssertEqual(Normalization.postalNormalize("  07000  "), "07000")
    }

    func testSearchKeyStripsAllSeparators() {
        XCTAssertEqual(Normalization.searchKeyUnaccented("Atatürk Caddesi"), "ataturkcaddesi")
        XCTAssertEqual(Normalization.searchKeyUnaccented("New York"), "newyork")
        XCTAssertEqual(Normalization.searchKeyUnaccented("東京都"), "東京都")
    }

    func testNoLatinishCombiningMarksSurvive() throws {
        // Narrower than "no combining marks at all", which would be wrong: a Japanese dakuten must
        // survive. See `core/spec/normalization.md` §2.
        let fixture = try Self.loadFixture()
        for vector in fixture.vectors {
            let result = Normalization.normalize(vector.input)
            var baseIsLatinish = false
            for scalar in result.unicodeScalars {
                let isMark = Normalization.isCombiningMarkForTesting(scalar)
                if isMark && baseIsLatinish {
                    XCTFail("vector #\(vector.n) kept a Latinish combining mark: \(result.debugDescription)")
                }
                if !isMark {
                    baseIsLatinish = Normalization.isLatinishBaseForTesting(scalar)
                }
            }
        }
    }
}

// MARK: - Test-visible wrappers
//
// The predicates are private because nothing in the SDK should depend on them, and the conformance
// test needs them. Exposing them for testing rather than for use keeps the public surface exactly
// what SDK_SPEC.md describes.
extension Normalization {
    static func isCombiningMarkForTesting(_ scalar: UnicodeScalar) -> Bool {
        isCombiningMark(scalar)
    }

    static func isLatinishBaseForTesting(_ scalar: UnicodeScalar) -> Bool {
        isLatinishBase(scalar)
    }
}