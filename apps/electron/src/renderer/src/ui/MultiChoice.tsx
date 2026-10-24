// Several options at once (a setting's or an export option's control "multi", docs/plugin-dev.md).

/** Several options at once (control "multi"): the value is the chosen values joined by commas. */
export function MultiChoice({ name, options, value, disabled, onChange }: { name: string; options: readonly { value: boolean | number | string; label: string }[]; value: string; disabled?: boolean; onChange: (v: string) => void }) {
  const on = new Set(value.split(",").filter(Boolean));
  const toggle = (v: string) => {
    const next = new Set(on);
    if (next.has(v)) next.delete(v);
    else next.add(v);
    onChange(options.map((o) => String(o.value)).filter((x) => next.has(x)).join(","));
  };
  return (
    <div className="set-multi" role="group" aria-label={name}>
      {options.map((o) => (
        <label key={String(o.value)} className={`set-chip${on.has(String(o.value)) ? " on" : ""}`}>
          <input type="checkbox" checked={on.has(String(o.value))} disabled={disabled} onChange={() => toggle(String(o.value))} />
          {o.label}
        </label>
      ))}
    </div>
  );
}
