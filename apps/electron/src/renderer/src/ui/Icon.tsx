import { icons, iconStrokeWidth, iconViewBox } from "@design/icons";

export type IconName = keyof typeof icons;

export function Icon({ name, size = 16, strokeWidth }: { name: IconName; size?: number; strokeWidth?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox={iconViewBox}
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth ?? iconStrokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: icons[name] }}
    />
  );
}

export function Kbd({ children }: { children: React.ReactNode }) {
  return <kbd className="kbd">{children}</kbd>;
}
