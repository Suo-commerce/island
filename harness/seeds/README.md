# Seed archive

Every case that ever broke a harness oracle is kept here, shrunk, with the
oracle it broke. `just harness` runs all of them before any new seed, and
`just test` runs them too. Nothing is ever removed from this directory.

One file per failure, named `<oracle>-seed-<seed>.json`:

- `oracle`: which oracle broke (`1-chain`, `2-replay`, `3-model`,
  `4-durability`, `7-retry` or `determinism`)
- `seed`: the seed that first produced the case
- `step` and `message`: where in the shrunk case it broke, and what was seen
- `ops`: the shrunk case itself

The operations are stored, not only the seed, so a case keeps testing what
it once caught after the generators change.

When the harness finds a failure it writes the file here and fails. Commit
the file together with the fix.

Empty so far: no seed has failed on committed code.
