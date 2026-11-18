import { Link, useLocation, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useFlare } from "./store";
import { LinkMark, Trust } from "./marks";
import { logHead } from "@/lib/mock";
import { Button } from "@/components/ui/button";
import { radialSelectionRotation } from "@/lib/radial-menu";

const nav = [
  { to: "/", label: "Now" },
  { to: "/session", label: "Session" },
  { to: "/sing", label: "Sing" },
  { to: "/agent", label: "Agent" },
] as const;
const later = ["Streams", "Approvals"];

const polar = (r: number, deg: number): [number, number] => {
  const rad = ((deg - 90) * Math.PI) / 180;
  return [160 + r * Math.cos(rad), 160 + r * Math.sin(rad)];
};
function wedgePath(a0: number, a1: number, r0: number, r1: number) {
  const [x0, y0] = polar(r1, a0);
  const [x1, y1] = polar(r1, a1);
  const [x2, y2] = polar(r0, a1);
  const [x3, y3] = polar(r0, a0);
  return `M${x0.toFixed(1)},${y0.toFixed(1)} A${r1},${r1} 0 0 1 ${x1.toFixed(1)},${y1.toFixed(1)} L${x2.toFixed(1)},${y2.toFixed(1)} A${r0},${r0} 0 0 0 ${x3.toFixed(1)},${y3.toFixed(1)} Z`;
}

export function CommandsMenu() {
  const { waiting } = useFlare();
  const [open, setOpen] = useState(false);
  const [rotation, setRotation] = useState(-360 / 14);
  const [selected, setSelected] = useState(0);
  const [turning, setTurning] = useState(false);
  const navigationTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const navigate = useNavigate();
  const location = useLocation();
  const close = () => {
    if (navigationTimer.current !== null) clearTimeout(navigationTimer.current);
    navigationTimer.current = null;
    setTurning(false);
    setOpen(false);
  };
  useEffect(() => () => {
    if (navigationTimer.current !== null) clearTimeout(navigationTimer.current);
  }, []);
  const items: { to?: string; label: string; badge?: number }[] = [
    { to: "/", label: "Now", badge: waiting.length },
    { to: "/session", label: "Session" },
    { to: "/sing", label: "Sing" },
    { label: "Streams" },
    { to: "/agent", label: "Agent" },
    { label: "Approvals" },
    { to: "/island", label: "Island" },
  ];
  const step = 360 / items.length;
  const gap = 1.6;
  const R0 = 64;
  const R1 = 152;
  const select = (index: number) => {
    const item = items[index];
    if (!item?.to || turning) return;
    const destination = item.to;
    setSelected(index);
    setRotation((current) => radialSelectionRotation(current, index, items.length));
    setTurning(true);
    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    navigationTimer.current = setTimeout(() => {
      navigationTimer.current = null;
      setTurning(false);
      setOpen(false);
      void navigate({ to: destination });
    }, reducedMotion ? 0 : 850);
  };
  return (
    <div>
      <Button onClick={() => {
        const index = Math.max(0, items.findIndex((item) => item.to === location.pathname));
        setSelected(index);
        setRotation(radialSelectionRotation(0, index, items.length));
        setOpen(true);
      }} className="h-auto w-full rounded-sm bg-gold py-3 font-display text-[13px] font-semibold tracking-[0.18em] text-gold-foreground uppercase underline underline-offset-4 hover:bg-gold/90">
        Commands
      </Button>
      {open && (
        <div role="dialog" aria-modal="true" aria-label="Commands" className="fixed inset-0 z-50 flex items-center justify-center bg-command-backdrop/95 backdrop-blur-sm" onClick={close} onKeyDown={(e) => { if (e.key === "Escape") close(); }}>
          <div className="relative h-[320px] w-[320px] max-w-[calc(100vw-24px)]" onClick={(e) => e.stopPropagation()}>
          <svg viewBox="0 0 320 320" className="radial-spin-in h-full w-full">
            <g className="radial-wheel" transform={`rotate(${rotation} 160 160)`}>
            {items.map((n, i) => {
              const a0 = i * step + gap;
              const a1 = (i + 1) * step - gap;
              const mid = (a0 + a1) / 2;
              const [lx, ly] = polar((R0 + R1) / 2, mid);
              const enabled = Boolean(n.to);
              return (
                <g key={n.label}
                  role="button" aria-label={n.label} aria-disabled={!enabled || turning}
                  tabIndex={enabled ? 0 : -1}
                  className={enabled ? "cursor-pointer outline-none" : "cursor-default opacity-35"}
                  onClick={() => select(i)}
                  onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select(i); } }}>
                  <path d={wedgePath(a0, a1, R0, R1)}
                    className={enabled ? `${selected === i ? "fill-gold/20 stroke-gold" : "fill-command-surface stroke-gold/70"} stroke-[1.5] transition-colors hover:fill-gold/20 active:fill-gold/30` : "fill-command-surface/60 stroke-command-text/20 stroke-[1.5]"} />
                  <g className="radial-label pointer-events-none" transform={`rotate(${-rotation} ${lx} ${ly})`}>
                  <text x={lx} y={ly - 4} textAnchor="middle"
                    className={`fill-command-text font-display text-[12px] font-semibold uppercase ${enabled ? "" : "opacity-50"}`}>
                    {n.label}
                  </text>
                  <text x={lx} y={ly + 13} textAnchor="middle"
                    className="fill-command-text/60 font-mono text-[10px]">
                    {n.badge && n.badge > 0 ? n.badge : enabled ? "open" : "soon"}
                  </text>
                  </g>
                </g>
              );
            })}
            </g>
            <circle cx={160} cy={160} r={56} className="fill-command-surface stroke-gold stroke-2" />
          </svg>
          <svg aria-hidden="true" viewBox="0 0 24 24" className="pointer-events-none absolute -top-3 left-1/2 h-6 w-6 -translate-x-1/2 fill-gold"><path d="M4 4h16L12 20Z" /></svg>
          <Button autoFocus variant="ghost" size="icon" aria-label="Close commands" onClick={close} className="absolute top-1/2 left-1/2 h-16 w-16 -translate-x-1/2 -translate-y-1/2 rounded-full text-2xl text-command-text hover:bg-command-text/10 hover:text-command-text">×</Button>
          </div>
        </div>
      )}
    </div>
  );
}

/** This page's name in the session; a click changes it. */
function ClientName() {
  const { client, rename, live } = useFlare();
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(client);
  if (!live) return <div>this device: laptop a3f09c1e</div>;
  if (!editing)
    return (
      <button title="This page's name in the session. Click to change it." className="block text-left underline-offset-2 hover:underline"
        onClick={() => { setName(client); setEditing(true); }}>
        this page: {client}
      </button>
    );
  return (
    <form onSubmit={(e) => { e.preventDefault(); rename(name); setEditing(false); }}>
      <input autoFocus aria-label="This page's name in the session" value={name} maxLength={20}
        onChange={(e) => setName(e.target.value)} onBlur={() => setEditing(false)}
        onKeyDown={(e) => { if (e.key === "Escape") setEditing(false); }}
        className="w-full rounded-sm border border-input bg-card px-1.5 py-0.5 text-foreground focus:outline-none focus:ring-1 focus:ring-ring" />
    </form>
  );
}

export function Shell({ children }: { children: ReactNode }) {
  const { waiting, head, link, live } = useFlare();
  const [dark, setDark] = useState(false);
  useEffect(() => {
    setDark(window.matchMedia("(prefers-color-scheme: dark)").matches);
  }, []);
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
  }, [dark]);

  return (
    <div className="min-h-screen md:grid md:grid-cols-[196px_1fr]">
      <aside className="hidden border-r-[3px] border-gold/70 bg-sidebar md:flex md:flex-col md:sticky md:top-0 md:h-screen">
        <div className="flex items-baseline gap-2 px-4 pt-5 pb-6">
          <span className="font-display text-[17px] font-semibold tracking-[0.12em]">FLARE</span>
          <span className="font-mono text-[11px] text-muted-foreground">/ island</span>
        </div>
        <nav className="flex flex-col gap-px px-2">
          {nav.map((n) => (
            <Link key={n.to} to={n.to} activeOptions={{ exact: true }}
              className="flex items-center justify-between rounded-sm px-2 py-1.5 text-[13px] text-sidebar-foreground/75 hover:bg-sidebar-accent data-[status=active]:bg-sidebar-accent data-[status=active]:text-sidebar-foreground">
              {n.label}
              {n.to === "/" && waiting.length > 0 && <span className="font-mono text-[11px] text-primary">{waiting.length}</span>}
            </Link>
          ))}
          {later.map((l) => (
            <span key={l} className="px-2 py-1.5 text-[13px] text-muted-foreground/50" title="Not built yet">{l}</span>
          ))}
        </nav>
        <div className="mx-2 mt-6 border-t border-sidebar-border pt-3">
          <Link to="/island" className="flex items-center justify-between rounded-sm px-2 py-1.5 text-[13px] text-sidebar-foreground/75 hover:bg-sidebar-accent data-[status=active]:bg-sidebar-accent data-[status=active]:text-sidebar-foreground">
            Island <span className="font-mono text-[10px] text-muted-foreground">system</span>
          </Link>
        </div>
        <div className="mt-auto space-y-1.5 border-t border-sidebar-border px-4 py-3 font-mono text-[11px] text-muted-foreground">
          <div><LinkMark state={link.state} reason={link.state === "offline" ? link.reason : undefined} /></div>
          <div>head {head === null ? "not read" : `#${head}`}</div>
          <ClientName />
          <button onClick={() => setDark((d) => !d)} className="pt-1 underline-offset-2 hover:underline">{dark ? "light" : "dark"} theme</button>
        </div>
      </aside>

      <div className="flex min-h-screen flex-col pb-14 md:pb-0">
        <header className="sticky top-0 z-20 flex items-center justify-between border-b border-border bg-background/95 px-4 py-2.5 backdrop-blur md:hidden">
          <span className="font-display text-[15px] font-semibold tracking-[0.12em]">FLARE</span>
          <div className="flex items-center gap-3">
            {link.state !== "attached" && <span className="font-mono text-[11px] text-muted-foreground"><LinkMark state={link.state} /></span>}
            <span className="font-mono text-[11px] text-muted-foreground">{head === null ? "" : `#${head}`}</span>
            <Trust verified={live ? false : logHead.verified} />
            <button onClick={() => setDark((d) => !d)} className="font-mono text-[11px] text-muted-foreground">{dark ? "light" : "dark"}</button>
          </div>
        </header>
        <div className="px-3 pt-3 md:hidden"><CommandsMenu /></div>
        <main className="flex-1">{children}</main>
      </div>

      <nav className="fixed inset-x-0 bottom-0 z-20 grid grid-cols-3 border-t border-border bg-background/95 backdrop-blur md:hidden">
        {nav.slice(0, 2).map((n) => (
          <Link key={n.to} to={n.to} activeOptions={{ exact: true }}
            className="flex items-center justify-center gap-1.5 py-3.5 text-[13px] text-muted-foreground data-[status=active]:text-foreground data-[status=active]:font-medium">
            {n.label}
            {n.to === "/" && waiting.length > 0 && <span className="font-mono text-[11px] text-primary">{waiting.length}</span>}
          </Link>
        ))}
        <Link to="/island" className="flex items-center justify-center py-3.5 text-[13px] text-muted-foreground data-[status=active]:text-foreground data-[status=active]:font-medium">Island</Link>
      </nav>
    </div>
  );
}
