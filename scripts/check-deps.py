#!/usr/bin/env python3
"""The workspace's dependency rules (docs/spec.md, "Repo and dependency direction").

1. No island-* crate depends on a flare-* crate.
2. flare-* crates reach Island only through island-core, island-log and
   island-client.
3. island-gate and island-model depend on island-core and nothing else, so
   the judge and the reference model stay pure (no async, no I/O).

Rules 1 and 2 cover every kind of dependency, dev and build included, so
either half can still be lifted out with its tests.  Rule 3 covers normal
and build dependencies; tests there may use dev-dependencies.

The rules go by crate name, so a new crate is covered the day it is added:
a flare-* crate may not use a new island-* crate until it is listed in
FLARE_MAY_USE.  The check reads the dependencies each crate declares, which
includes optional and target-specific ones.

Run by `just deps`.  Exit code 1 lists every violation.
"""

import json
import subprocess
import sys

FLARE_MAY_USE = {"island-core", "island-log", "island-client"}
ONLY_ISLAND_CORE = {"island-gate", "island-model"}


def main() -> int:
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    members = {package["name"]: package for package in metadata["packages"]}

    errors = []
    for name in sorted(ONLY_ISLAND_CORE - members.keys()):
        errors.append(f"{name}: named in rule 3 but not in the workspace")

    for name, package in sorted(members.items()):
        half = name.split("-", 1)[0]
        if half not in ("island", "flare"):
            errors.append(
                f"{name}: workspace crates are named island-* or flare-*; "
                "decide which half this crate belongs to"
            )
            continue
        for dep in package["dependencies"]:
            dep_name = dep["name"]
            kind = dep["kind"] or "normal"
            if half == "island" and dep_name.startswith("flare-"):
                errors.append(
                    f"{name} -> {dep_name} ({kind}): rule 1, "
                    "no island-* crate depends on a flare-* crate"
                )
            if half == "flare" and dep_name.startswith("island-") and dep_name not in FLARE_MAY_USE:
                errors.append(
                    f"{name} -> {dep_name} ({kind}): rule 2, flare-* reaches Island "
                    "only through " + ", ".join(sorted(FLARE_MAY_USE))
                )
            if name in ONLY_ISLAND_CORE and kind != "dev" and dep_name != "island-core":
                errors.append(
                    f"{name} -> {dep_name} ({kind}): rule 3, "
                    f"{name} depends only on island-core"
                )

    if errors:
        print("check-deps: dependency rules broken:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    print(f"check-deps: {len(members)} crates, all three dependency rules hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
