import { useEffect } from "react";

/** The tab's title, for the screen that is open. */
export function useTitle(screen: string): void {
  useEffect(() => {
    document.title = `${screen} · Flare`;
  }, [screen]);
}
