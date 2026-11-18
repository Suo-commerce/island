// The Flare terminal's command for a verse (crates/flare-tui, `/verse`):
//   /verse @STREAM EVIDENCE TEXT
// The terminal reads one line, so the text goes on one.

/** The command that sings `text` on `stream`, or null while something is missing. */
export function verseCommand(stream: string, evidence: string, text: string): string | null {
  const line = text.split(/\s*\n\s*/).map((part) => part.trim()).filter(Boolean).join(" ");
  if (!stream || !evidence || !line) return null;
  return `/verse @${stream} ${evidence} ${line}`;
}

/**
 * Copy `text`. The page is served over plain http inside the tunnel, where
 * a browser offers no clipboard API; the old way through a selection still
 * works there. False: it could not be copied.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to the selection.
  }
  const holder = document.createElement("textarea");
  holder.value = text;
  holder.setAttribute("readonly", "");
  holder.style.position = "fixed";
  holder.style.opacity = "0";
  document.body.appendChild(holder);
  holder.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  holder.remove();
  return copied;
}
