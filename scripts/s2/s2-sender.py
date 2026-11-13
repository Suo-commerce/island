#!/usr/bin/env python3
"""The S2 spike's sender: on the host, on the Warden's sender socket
(<run_dir>/senders/<vessel-id>.sock, root only).  Prints one JSON
document: the Open list before, what was reported, and Open after.

    s2-sender.py --socket PATH [--apply] [--lapse-unknown]

  (no option)       only list what is released and not settled
  --apply           report Applied for every listed effect that is not
                    unknown (a stand-in for "the broker approved it and
                    it was sent")
  --lapse-unknown   report NotApplied for every effect marked unknown
                    (the lapse rule of decision D1: after a Warden
                    restart, an effect not yet approved is declined and
                    the agent proposes it again)

This is the reference sender for runs a to f; island-sender replaces it
in S2.4.
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s2_field as f  # noqa: E402

LAPSE_REASON = "lapsed: Warden restarted before approval"


def main():
    p = argparse.ArgumentParser(description="S2 spike sender (host)")
    p.add_argument("--socket", required=True)
    p.add_argument("--apply", action="store_true")
    p.add_argument("--lapse-unknown", action="store_true")
    args = p.parse_args()

    field = f.Field(args.socket)
    before = field.ask(f.open_())
    if before["type"] != "Open":
        raise SystemExit(f"unexpected answer to Open: {before}")
    reported = []
    for e in before["effects"]:
        if e["unknown"] and args.lapse_unknown:
            r = field.ask(f.outcome_not_applied(e["op"], LAPSE_REASON))
            reported.append({"op": e["op"], "outcome": "NotApplied", "refused": r.get("refused")})
        elif not e["unknown"] and args.apply:
            r = field.ask(f.outcome_applied(e["op"], f"s2-receipt-{e['op']}".encode()))
            reported.append({"op": e["op"], "outcome": "Applied", "refused": r.get("refused")})
    after = field.ask(f.open_())
    field.close()
    json.dump({"before": before["effects"], "reported": reported, "after": after["effects"]},
              sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
