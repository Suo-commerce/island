import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Flame, X } from "lucide-react";
import { useFlare } from "./store";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Drawer, DrawerContent, DrawerDescription, DrawerHeader, DrawerTitle } from "@/components/ui/drawer";
import { useIsMobile } from "@/hooks/use-mobile";
import type { Waiting } from "@/lib/view";
import { Dot } from "./marks";

const typeLabel = { action: "action · verse", nudge: "repo nudge", verdict: "refused verse", question: "agent question" };

export function WaitingList() {
  const { waiting, settled } = useFlare();
  return (
    <div>
      {waiting.length === 0 && <p className="py-6 text-center text-[13px] text-muted-foreground">Nothing waits for you.</p>}
      <ul className="divide-y divide-border">
        {waiting.map((w) => <WaitingItem key={w.id} w={w} />)}
      </ul>
      {settled.length > 0 && (
        <ul className="mt-3 space-y-1 border-t border-dashed border-border pt-2">
          {settled.map((s) => (
            <li key={s.id + s.time} className="flex gap-2 font-mono text-[11.5px] text-muted-foreground">
              <span>{s.time}</span><span className={s.outcome === "approved" || s.outcome === "sent" ? "text-verified" : ""}>{s.outcome}</span>
              <span className="truncate">{s.label}</span>{s.seq !== undefined && <span className="ml-auto">#{s.seq}</span>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Meta({ w }: { w: Waiting }) {
  return (
    <div className="mb-1.5 flex items-center gap-2 font-mono text-[11px] text-muted-foreground">
      <Dot tone={w.type === "verdict" ? "unverified" : "primary"} />
      <span className="text-foreground/80">{typeLabel[w.type]}</span>
      <span>{w.id}</span>
      <span className="ml-auto">{w.at}</span>
    </div>
  );
}

function WaitingItem({ w }: { w: Waiting }) {
  const { settle } = useFlare();
  const nav = useNavigate();
  const [confirm, setConfirm] = useState(false);
  const [editing, setEditing] = useState(false);

  if (w.type === "action")
    return (
      <li className="py-3">
        <Meta w={w} />
        <p className="text-[12px] text-muted-foreground">{w.stream ? <>agent wants to write to <span className="font-mono text-foreground">{w.stream}</span></> : "agent wants to send"}</p>
        <blockquote className="my-2 border-l-2 border-primary/60 bg-muted/50 px-3 py-2 text-[13.5px] leading-snug">{w.text}</blockquote>
        <dl className="mb-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 font-mono text-[11px] text-muted-foreground">
          <dt>provider</dt><dd className="text-foreground/80">{w.provider}</dd>
          <dt>req</dt><dd className="text-foreground/80">{w.hash}</dd>
          <dt>from</dt><dd className="text-foreground/80">{w.fork}</dd>
        </dl>
        {w.event !== undefined ? (
          <p className="rounded-sm border border-border bg-card px-3 py-2 text-[12.5px] text-muted-foreground">
            This page holds no key yet, so it cannot answer. In the terminal:{" "}
            <span className="font-mono text-foreground">/approve {w.event}</span> or <span className="font-mono text-foreground">/decline {w.event}</span>.
            It shows here as soon as it is settled.
          </p>
        ) : (
        <div className="grid grid-cols-2 gap-2 sm:flex">
          <Button size="lg" className="sm:h-8 sm:px-4" onClick={() => setConfirm(true)}><Flame className="size-4" />Approve…</Button>
          <Button size="lg" variant="outline" className="sm:h-8 sm:px-4" onClick={() => settle(w.id, "declined", `verse→${w.stream} ${w.hash}`, { device: "phone", type: "settled", text: `declined verse→${w.stream} · never sent` })}><X className="size-4" />Decline</Button>
        </div>
        )}
        <ApproveConfirm open={confirm} onOpenChange={setConfirm} w={w}
          onApprove={(device) => settle(w.id, "approved", `verse→${w.stream} ${w.hash}`, { device, type: "settled", text: `approved verse→${w.stream} (req ${w.hash})` })} />
      </li>
    );

  if (w.type === "nudge")
    return (
      <li className="py-3">
        <Meta w={w} />
        <p className="mb-3 text-[13.5px]">repo <span className="font-mono">{w.repo}</span> has <span className="font-mono">{w.commits}</span> commits since the last verse, {w.since}.</p>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="outline" onClick={() => settle(w.id, "drafting", `agent drafts verse for ${w.repo}`, { device: "laptop", type: "cmd", text: `/draft verse ${w.repo} --from-receipts` })}>Ask agent to draft</Button>
          <Button size="sm" variant="outline" disabled title="Composer not in this prototype yet">Sing a verse myself</Button>
          <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => settle(w.id, "dismissed", `nudge ${w.repo}`, { device: "laptop", type: "result", text: `nudge ${w.repo} dismissed` })}>Dismiss</Button>
        </div>
      </li>
    );

  if (w.type === "verdict")
    return (
      <li className="py-3">
        <Meta w={w} />
        <p className="text-[13.5px]">gate refused a verse to <span className="font-mono">{w.stream}</span>: <span className="text-unverified">{w.reason}</span></p>
        {editing ? (
          <textarea defaultValue={w.draft} rows={4} className="mt-2 w-full rounded-sm border border-input bg-card p-2 text-[13px] focus:outline-none focus:ring-1 focus:ring-ring" />
        ) : (
          <p className="mt-1.5 truncate text-[12.5px] text-muted-foreground">“{w.draft}”</p>
        )}
        <div className="mt-3 flex flex-wrap gap-2">
          {editing ? (
            <Button size="sm" onClick={() => settle(w.id, "approved", `verse→${w.stream} resent`, { device: "laptop", type: "settled", text: `verse→${w.stream} resent, accepted by gate` })}>Resend</Button>
          ) : (
            <Button size="sm" variant="outline" onClick={() => setEditing(true)}>Edit and resend</Button>
          )}
          <Button size="sm" variant="ghost" className="text-muted-foreground" title="Recorded and counted. Lets nothing through."
            onClick={() => settle(w.id, "override requested", `${w.stream} verdict (counted, nothing passed)`, { device: "laptop", type: "result", text: `override requested on ${w.stream} verdict · recorded · nothing passed` })}>Request override</Button>
        </div>
      </li>
    );

  return (
    <li className="py-3">
      <Meta w={w} />
      <p className="mb-2 text-[13.5px]">{w.text}</p>
      <Button size="sm" variant="outline" onClick={() => nav({ to: "/session" })}>Answer in session</Button>
    </li>
  );
}

function ApproveConfirm({ open, onOpenChange, w, onApprove }: { open: boolean; onOpenChange: (o: boolean) => void; w: Extract<Waiting, { type: "action" }>; onApprove: (d: "phone" | "laptop") => void }) {
  const mobile = useIsMobile();
  const [step, setStep] = useState<"review" | "passkey">("review");
  const close = (o: boolean) => { onOpenChange(o); if (!o) setStep("review"); };
  const body = (
    <div className="space-y-3 text-[13px]">
      <p className="text-muted-foreground">This will be sent exactly once, signed as an approval by <span className="font-mono text-foreground">{mobile ? "7b21de04 phone" : "a3f09c1e laptop"}</span>.</p>
      <div className="rounded-sm border border-border bg-muted/40 p-3 font-mono text-[12px] leading-relaxed">
        <div className="text-muted-foreground">verse → {w.stream} · req {w.hash}</div>
        <div className="mt-1 font-sans text-[13px] text-foreground">{w.text}</div>
      </div>
    </div>
  );

  if (mobile)
    return (
      <Drawer open={open} onOpenChange={close}>
        <DrawerContent>
          <DrawerHeader className="text-left">
            <DrawerTitle className="text-[15px] font-medium">{step === "review" ? "Approve this verse?" : "Confirm with passkey"}</DrawerTitle>
            <DrawerDescription className="sr-only">Approve agent action</DrawerDescription>
          </DrawerHeader>
          <div className="px-4 pb-6">
            {step === "review" ? (
              <>
                {body}
                <div className="mt-5 grid gap-2">
                  <Button size="lg" className="h-12" onClick={() => setStep("passkey")}><Flame className="size-4" />Approve and send once</Button>
                  <Button size="lg" variant="ghost" className="h-12" onClick={() => close(false)}>Not now</Button>
                </div>
              </>
            ) : (
              <div className="flex flex-col items-center gap-4 py-4">
                <svg width="52" height="52" viewBox="0 0 52 52" className="text-primary" aria-hidden>
                  <circle cx="26" cy="26" r="24" fill="none" stroke="currentColor" strokeWidth="1.2" strokeDasharray="3 3" className="origin-center animate-[spin_8s_linear_infinite]" />
                  <path d="M20 22a6 6 0 1 1 12 0v4M18 26h16v10H18z" fill="none" stroke="currentColor" strokeWidth="1.5" />
                </svg>
                <p className="text-center text-[13px] text-muted-foreground">Use Face ID or your device passkey for flare · island</p>
                <Button size="lg" className="h-12 w-full" onClick={() => { onApprove("phone"); close(false); }}>Simulate passkey</Button>
                <Button variant="ghost" onClick={() => setStep("review")}>Back</Button>
              </div>
            )}
          </div>
        </DrawerContent>
      </Drawer>
    );

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="text-[15px] font-medium">Approve this verse?</DialogTitle>
          <DialogDescription className="sr-only">Approve agent action</DialogDescription>
        </DialogHeader>
        {body}
        <DialogFooter>
          <Button variant="ghost" onClick={() => close(false)}>Cancel</Button>
          <Button onClick={() => { onApprove("laptop"); close(false); }}><Flame className="size-4" />Approve and send once</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
