"""Golden vectors for s2_field.py, made by Taso's own Rust codec
(taso-field, Taso c031ee5) with the same values.  Run: python3 -m unittest
scripts/s2/test_s2_field.py, or `just s2-test`."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s2_field as f  # noqa: E402

VECTORS = {
    "req_hello": (
        "000200"
    ),
    "req_effect_irr": (
        "01e903000000000000060000000000000069736c616e64030000000000000072"
        "65710000000000000000001000000000000000ababababababababababababab"
        "ababab0002000000000000007332"
    ),
    "req_effect_nem": (
        "010700000000000000060000000000000069736c616e64010000000000000078"
        "000000000000000000020000000000000001020202000000000000007332"
    ),
    "req_transcript": (
        "02010000000000000074"
    ),
    "req_turnend": (
        "03030000000000000001"
    ),
    "req_applied": (
        "04090000000000000000010000000000000072"
    ),
    "req_notapplied": (
        "0409000000000000000106000000000000006c6170736564"
    ),
    "req_open": (
        "05"
    ),
    "rep_hello": (
        "000200010400000000000000"
    ),
    "rep_released": (
        "010500000000000000004d00000000000000"
    ),
    "rep_held": (
        "010500000000000000014e00000000000000"
    ),
    "rep_refused": (
        "010500000000000000022b000000000000006e6f206566666563742061757468"
        "6f726974793a203120756e7265736f6c76656420656666656374287329"
    ),
    "rep_recorded": (
        "020c00000000000000"
    ),
    "rep_turnend": (
        "03020000000000000000"
    ),
    "rep_outcome": (
        "0509000000000000000102000000000000006e6f"
    ),
    "rep_open": (
        "0601000000000000000300000000000000060000000000000069736c616e6401"
        "0000000000000071001000000000000000cdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"
        "cd000200000000000000733201"
    ),
}


def v(name):
    return bytes.fromhex(VECTORS[name])


class Requests(unittest.TestCase):
    def test_encodings_match_taso(self):
        self.assertEqual(f.hello(), v("req_hello"))
        self.assertEqual(f.effect(1001, "island", b"req", b"\xab" * 16, f.IRREVERSIBLE), v("req_effect_irr"))
        self.assertEqual(f.effect(7, "island", b"x", b"\x01\x02", f.NO_EXTERNAL_MUTATION), v("req_effect_nem"))
        self.assertEqual(f.transcript(b"t"), v("req_transcript"))
        self.assertEqual(f.turn_end(3, True), v("req_turnend"))
        self.assertEqual(f.outcome_applied(9, b"r"), v("req_applied"))
        self.assertEqual(f.outcome_not_applied(9, "lapsed"), v("req_notapplied"))
        self.assertEqual(f.open_(), v("req_open"))


class Replies(unittest.TestCase):
    def test_decodings_match_taso(self):
        self.assertEqual(f.decode_reply(v("rep_hello")), {"type": "Hello", "version": 2, "last_sealed_turn": 4})
        self.assertEqual(f.decode_reply(v("rep_released")), {"type": "Effect", "call": 5, "result": "Released", "op": 77})
        self.assertEqual(f.decode_reply(v("rep_held")), {"type": "Effect", "call": 5, "result": "Held", "op": 78})
        self.assertEqual(f.decode_reply(v("rep_refused")),
                         {"type": "Effect", "call": 5, "result": "Refused",
                          "reason": "no effect authority: 1 unresolved effect(s)"})
        self.assertEqual(f.decode_reply(v("rep_recorded")), {"type": "Recorded", "lsn": 12})
        self.assertEqual(f.decode_reply(v("rep_turnend")), {"type": "TurnEnd", "turn": 2, "sealed": None})
        self.assertEqual(f.decode_reply(v("rep_outcome")), {"type": "Outcome", "op": 9, "refused": "no"})
        self.assertEqual(f.decode_reply(v("rep_open")), {"type": "Open", "effects": [{
            "op": 3, "provider": "island", "request": "q",
            "retry": ["ProviderDeduplicated", "cd" * 16], "consequence": f.IRREVERSIBLE,
            "authority": "s2", "unknown": True}]})

    def test_trailing_bytes_are_refused(self):
        with self.assertRaises(ValueError):
            f.decode_reply(v("rep_recorded") + b"\x00")

    def test_truncation_is_refused(self):
        with self.assertRaises(ValueError):
            f.decode_reply(v("rep_open")[:-1])


if __name__ == "__main__":
    unittest.main()
