import type { ReactNode } from 'react';
import { Label } from '@/components/ui/label';

/** One line of a settings list: label and optional hint on the left, the control on the right. */
export function SettingRow({ id, label, hint, children }: { id?: string; label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-6 px-4 py-3">
      <div className="min-w-0 space-y-0.5">
        <Label htmlFor={id} className="text-sm font-medium">{label}</Label>
        {hint ? <p className="text-[13px] leading-snug text-muted-foreground">{hint}</p> : null}
      </div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}

export function Section({ id, title, children }: { id?: string; title: string; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-6 space-y-2">
      <h2 className="text-sm font-medium">{title}</h2>
      <div className="divide-y rounded-lg border bg-card text-card-foreground">{children}</div>
    </section>
  );
}
