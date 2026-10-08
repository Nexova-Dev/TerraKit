#!/usr/bin/env python3
"""
Run the shared conformance battery and produce, or check, the expected results.

`SDK_SPEC.md` section 14 requires the same query battery against the same package bytes in the CLI,
Swift and Kotlin, comparing **result ID sequences and orderings** rather than counts. A count can
match while the order does not, and the user experiences that as "why is the right answer fourth".

The Rust CLI is the reference implementation: it is the one that produced the packages, so if two
implementations disagree the CLI's ordering is the one that is correct by construction — and if it is
not, that is a finding about the CLI.

Usage
-----
    tools/conformance.py generate --packages <dir>       # write expected/*.json
    tools/conformance.py check   --packages <dir>       # run the CLI against the expectations
    tools/conformance.py print   --packages <dir>       # what the CLI returns, for a diff

Exit codes: 0 pass, 1 divergence, 2 usage error.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "data-pipeline" / "src"))

BATTERY = REPO_ROOT / "tests" / "integration" / "conformance" / "battery.json"
EXPECTED = REPO_ROOT / "tests" / "integration" / "conformance" / "expected"
CLI = REPO_ROOT / "cli" / "target" / "release" / "terrakit"


def load_battery() -> dict:
    return json.loads(BATTERY.read_text(encoding="utf-8"))


def cli_binary() -> pathlib.Path:
    if not CLI.is_file():
        raise SystemExit(
            f"error: {CLI} does not exist.\n"
            "       cargo build --manifest-path cli/Cargo.toml --release"
        )
    return CLI


def project_for(packages: pathlib.Path, tmp: pathlib.Path) -> pathlib.Path:
    """Lay the package directories out the way `.terrakit/packages` expects.

    The CLI reads `.terrakit/packages/<CC>/<version>/`. Copying rather than symlinking because the
    CLI resolves the manifest by canonical path and a symlinked database would still verify, which
    would hide a layout problem this script is meant to catch.
    """
    import shutil

    project = tmp / "conformance-project"
    shutil.rmtree(project, ignore_errors=True)
    (project / ".terrakit" / "packages").mkdir(parents=True)

    for directory in sorted(packages.glob("terrakit-data-*")):
        parts = directory.name.split("-")
        country = parts[2].upper()
        version = directory.name.split("-", 3)[3]
        target = project / ".terrakit" / "packages" / country / version
        shutil.copytree(directory, target, dirs_exist_ok=True)

    subprocess.run([str(cli_binary()), "init"], cwd=project, check=True, capture_output=True)
    return project


def run_cli(project: pathlib.Path, query: dict) -> dict:
    """Run one battery query through the CLI and return its normalized result."""
    operation = query["op"]
    country = query.get("country")
    limit = query.get("limit", 20)

    if operation == "search":
        command = ["search", query["query"], "--json"]
        if country:
            command += ["--country", country]
        command += ["--limit", str(limit)]
    elif operation == "searchCity":
        command = ["search", query["query"], "--type", "city,town", "--json"]
        if country:
            command += ["--country", country]
        command += ["--limit", str(limit)]
    elif operation == "country":
        command = ["info", country, "--json"]
    elif operation == "postalCode":
        command = ["postal", country, query["code"], "--json"]
    elif operation == "nearest":
        # The CLI has no `nearest` command; `search` is the only nearest-adjacent read it exposes.
        # Recorded as unavailable rather than silently skipped, so a future implementation knows it
        # was never checked.
        return {"ids": [], "unavailable": "the CLI exposes no nearest query"}
    else:
        # autocomplete, ancestors, children, localizedName have no CLI equivalent either: they are
        # SDK surface. The expectations for those are produced by the first SDK to run them and are
        # then held by the others.
        return {"ids": [], "unavailable": f"no CLI equivalent for '{operation}'"}

    result = subprocess.run(
        [str(cli_binary()), *command],
        cwd=project,
        capture_output=True,
        text=True,
    )
    if result.returncode not in (0, 4):
        return {"ids": [], "error": result.stderr.strip()[:200]}

    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"ids": [], "error": "output was not JSON"}

    if operation in ("search", "searchCity"):
        return {"ids": [row["id"] for row in payload["results"]]}

    if operation == "country":
        # Emitted as `ids` like every other query, so one comparison covers all of them. These four
        # were previously recorded under `fields`, which the SDK suites did not read -- so the country
        # row was the one part of the surface that no SDK was actually verified on, and the suite
        # reported full coverage while skipping it. A field-by-field list, in a fixed order, names the
        # divergence instead of counting matches.
        # Null becomes "-" rather than Python's "None", which would make the expected value depend
        # on the language that generated it. Georgia has no driving side in the source data, and a
        # suite whose oracle says "None" is a suite that Kotlin's `null` and Swift's `nil` can never
        # match.
        def token(value):
            return "-" if value is None else str(value)

        return {"ids": [
            token(payload["iso2"]),
            token(payload["iso3"]),
            token(payload["name"]),
            token(payload["capital"]),
            token(payload["currency"]),
            token(payload["drivingSide"]),
            token(payload["level"]),
            token(payload["places"]),
        ]}

    if operation == "postalCode":
        # Ids, not names: a name is a weaker key than an id, and two districts can share one.
        return {
            "ids": payload.get("areaIds", []),
            "names": payload.get("areas", []),
            "fields": {
                "code": payload["code"],
                "country": payload["country"],
                "primary": payload.get("placeId"),
                "areaCount": payload["areaCount"],
            },
        }

    return {"ids": []}


def generate(packages: pathlib.Path, tmp: pathlib.Path) -> int:
    battery = load_battery()
    project = project_for(packages, tmp)

    EXPECTED.mkdir(parents=True, exist_ok=True)
    results = {}
    for query in battery["queries"]:
        results[query["id"]] = {
            **run_cli(project, query),
            "why": query.get("why", ""),
        }

    document = {
        "$comment": (
            "Produced by tools/conformance.py from the Rust CLI, which is the reference "
            "implementation. Regenerate with --generate only when a change to ordering is "
            "intended: regenerating to make a test pass would destroy the only thing the suite "
            "exists to check. See CONTRIBUTING.md section 5.1."
        ),
        "batteryVersion": battery["version"],
        "packages": sorted(
            d.name for d in packages.glob("terrakit-data-*")
        ),
        "results": results,
    }
    path = EXPECTED / "cli.json"
    path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    checked = sum(1 for r in results.values() if not r.get("unavailable"))
    print(f"wrote {path.relative_to(REPO_ROOT)}")
    print(f"  {len(results)} queries, {checked} with a CLI equivalent, "
          f"{len(results) - checked} SDK-only")
    return 0


def check(packages: pathlib.Path, tmp: pathlib.Path) -> int:
    path = EXPECTED / "cli.json"
    if not path.is_file():
        print(f"error: {path} does not exist; run --generate first", file=sys.stderr)
        return 2

    battery = load_battery()
    expected = json.loads(path.read_text(encoding="utf-8"))["results"]
    project = project_for(packages, tmp)

    failures = []
    for query in battery["queries"]:
        want = expected.get(query["id"])
        if want is None:
            failures.append(f"{query['id']}: no expectation recorded")
            continue
        if want.get("unavailable"):
            continue
        got = run_cli(project, query)
        if got != {k: v for k, v in want.items() if k != "why"}:
            failures.append(
                f"{query['id']} ({query.get('why', '')}):\n"
                f"      expected {json.dumps({k: v for k, v in want.items() if k != 'why'})}\n"
                f"      actual   {json.dumps(got)}"
            )

    if failures:
        print(f"{len(failures)} divergence(s):", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(f"CLI matches the recorded expectations for every available query")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("mode", choices=("generate", "check", "print"))
    parser.add_argument("--packages", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)

    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        if args.mode == "generate":
            return generate(args.packages, tmp_path)
        if args.mode == "check":
            return check(args.packages, tmp_path)

        battery = load_battery()
        project = project_for(args.packages, tmp_path)
        for query in battery["queries"]:
            print(query["id"], json.dumps(run_cli(project, query), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())