# Flare's face

The page `flare-server` serves from itself (`--face`): one more client of
the shared session, beside the terminal. It started as Jarmo's prototype
in Lovable; since 2026-11-17 the code lives here.

```
src/lib/session.ts     the session protocol as the page speaks it; GET /log
src/lib/view.ts        events → what the screens show (pure, tested)
src/components/flare/  the shell, the stream, the waiting list, the marks;
                       store.tsx is the one seam between screens and data
src/routes/            the screens: index (Now), session, sing, agent, island
src/lib/*mock.ts       the prototype's sample data
dist/                  the built page, kept in the repo
```

**Live:** Now and Session. **Sample data, marked on the screen:** Sing,
Agent, Island. The page signs nothing and checks no signature yet, so
every mark reads "server says" (docs/slice-2.md, "The face, first cut").

## Working on it

```sh
just face          # npm ci, the tests, the build into face/dist
just face-stack    # the built page against a real local session; no server needed
```

or, in `face/`:

```sh
npm ci
npm run dev        # against a flare-server started with --face (FLARE_SERVER, default http://127.0.0.1:4749)
npm run dev:mock   # the whole page on sample data, as the prototype was
npm test
npm run build
```

`dist/` is committed so that the server needs no Node: after a change,
run `just face` and commit `dist/` with the source. `.npmrc` turns
package install scripts off; none of the dependencies needs one.

The page loads nothing from elsewhere: the fonts (IBM Plex Mono, Cinzel)
are files of the build, and `flare-server` sends a content security
policy that allows the page's own origin only.
