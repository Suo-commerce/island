import { Link, Outlet, createRootRoute, type ErrorComponentProps } from "@tanstack/react-router";

import { Shell } from "../components/flare/Shell";
import { FlareProvider } from "../components/flare/store";

function NotFound() {
  return (
    <div className="flex min-h-[60vh] items-center justify-center px-4">
      <div className="max-w-md text-center">
        <h1 className="font-display text-xl font-semibold tracking-[0.12em]">No such page</h1>
        <p className="mt-2 text-sm text-muted-foreground">Flare has no screen at this address.</p>
        <Link to="/" className="mt-6 inline-flex rounded-sm bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">
          To Now
        </Link>
      </div>
    </div>
  );
}

function Broken({ error, reset }: ErrorComponentProps) {
  console.error(error);
  return (
    <div className="flex min-h-screen items-center justify-center bg-background px-4">
      <div className="max-w-md text-center">
        <h1 className="font-display text-xl font-semibold tracking-[0.12em]">This screen broke</h1>
        <p className="mt-2 break-words font-mono text-[12px] text-muted-foreground">{String(error)}</p>
        <p className="mt-2 text-sm text-muted-foreground">Nothing was sent or changed by it. The session goes on; the terminal still works.</p>
        <button onClick={reset} className="mt-6 rounded-sm bg-primary px-4 py-2 text-sm font-medium text-primary-foreground">
          Try again
        </button>
      </div>
    </div>
  );
}

export const Route = createRootRoute({
  component: Root,
  notFoundComponent: NotFound,
  errorComponent: Broken,
});

function Root() {
  return (
    <FlareProvider>
      <Shell>
        <Outlet />
      </Shell>
    </FlareProvider>
  );
}
