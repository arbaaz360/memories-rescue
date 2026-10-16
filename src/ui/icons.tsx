/** Inline line icons. Decorative: always aria-hidden, the text next to them carries the meaning. */

import type { JSX } from "preact";

type IconProps = { size?: number; class?: string };

function Svg(props: IconProps & { children: JSX.Element | JSX.Element[] }) {
  const size = props.size ?? 20;
  return (
    <svg
      class={props.class ? `icon ${props.class}` : "icon"}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {props.children}
    </svg>
  );
}

export const IconFile = (p: IconProps) => (
  <Svg {...p}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M14 3v5h5" />
  </Svg>
);
export const IconZip = (p: IconProps) => (
  <Svg {...p}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M14 3v5h5" />
    <path d="M10 6h1M10 9h1M10 12h1M9.5 15h2v3h-2z" />
  </Svg>
);
export const IconFolder = (p: IconProps) => (
  <Svg {...p}>
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
  </Svg>
);
export const IconUpload = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 16V4M7 9l5-5 5 5" />
    <path d="M4 16v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" />
  </Svg>
);
export const IconCheck = (p: IconProps) => (
  <Svg {...p}>
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  </Svg>
);
export const IconWarning = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 3.5l9.5 16.5h-19z" />
    <path d="M12 10v4.5M12 17.5v.01" />
  </Svg>
);
export const IconError = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M9 9l6 6M15 9l-6 6" />
  </Svg>
);
export const IconInfo = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11v5.5M12 7.5v.01" />
  </Svg>
);
export const IconPin = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 21s-6.5-6.2-6.5-11.2a6.5 6.5 0 0 1 13 0C18.5 14.8 12 21 12 21z" />
    <circle cx="12" cy="9.8" r="2.3" />
  </Svg>
);
export const IconPinOff = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 21s-6.5-6.2-6.5-11.2a6.5 6.5 0 0 1 13 0C18.5 14.8 12 21 12 21z" />
    <path d="M4 4l16 16" />
  </Svg>
);
export const IconPhoto = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="5" width="18" height="14" rx="2" />
    <circle cx="9" cy="10" r="1.8" />
    <path d="M21 16l-5-5-8 8" />
  </Svg>
);
export const IconVideo = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="6" width="13" height="12" rx="2" />
    <path d="M16 10.5l5-3v9l-5-3z" />
  </Svg>
);
export const IconLayers = (p: IconProps) => (
  <Svg {...p}>
    <path d="M12 3l9 5-9 5-9-5z" />
    <path d="M3 13l9 5 9-5" />
  </Svg>
);
export const IconCalendar = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3.5" y="5" width="17" height="15" rx="2" />
    <path d="M3.5 10h17M8 3v4M16 3v4" />
  </Svg>
);
export const IconDisk = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="6" width="18" height="12" rx="2" />
    <path d="M7 14h.01M11 14h6" />
  </Svg>
);
export const IconClock = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7v5l3 2" />
  </Svg>
);
export const IconMoon = (p: IconProps) => (
  <Svg {...p}>
    <path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z" />
  </Svg>
);
export const IconCoffee = (p: IconProps) => (
  <Svg {...p}>
    <path d="M4 9h12v5a5 5 0 0 1-5 5H9a5 5 0 0 1-5-5z" />
    <path d="M16 10h1.5a2.5 2.5 0 0 1 0 5H16M8 3v3M12 3v3" />
  </Svg>
);
export const IconPause = (p: IconProps) => (
  <Svg {...p}>
    <path d="M9 5v14M15 5v14" />
  </Svg>
);
export const IconPlay = (p: IconProps) => (
  <Svg {...p}>
    <path d="M7 4.5l12 7.5-12 7.5z" />
  </Svg>
);
export const IconStop = (p: IconProps) => (
  <Svg {...p}>
    <rect x="6" y="6" width="12" height="12" rx="1.5" />
  </Svg>
);
export const IconExternal = (p: IconProps) => (
  <Svg {...p}>
    <path d="M14 4h6v6M20 4l-9 9" />
    <path d="M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4" />
  </Svg>
);
export const IconKey = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="8" cy="15" r="4" />
    <path d="M11 12l9-9M16 7l3 3M14 9l2 2" />
  </Svg>
);
export const IconGear = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="3" />
    <path d="M12 2.5v2.2M12 19.3v2.2M4.6 4.6l1.6 1.6M17.8 17.8l1.6 1.6M2.5 12h2.2M19.3 12h2.2M4.6 19.4l1.6-1.6M17.8 6.2l1.6-1.6" />
  </Svg>
);
export const IconHelp = (p: IconProps) => (
  <Svg {...p}>
    <circle cx="12" cy="12" r="9" />
    <path d="M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .9-1 1.6v.6M12 17v.01" />
  </Svg>
);
export const IconClose = (p: IconProps) => (
  <Svg {...p}>
    <path d="M6 6l12 12M18 6L6 18" />
  </Svg>
);
export const IconArrowBack = (p: IconProps) => (
  <Svg {...p}>
    <path d="M10 6l-6 6 6 6M4 12h16" />
  </Svg>
);
export const IconLink = (p: IconProps) => (
  <Svg {...p}>
    <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" />
    <path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" />
  </Svg>
);
export const IconLock = (p: IconProps) => (
  <Svg {...p}>
    <rect x="5" y="10.5" width="14" height="10" rx="2" />
    <path d="M8 10.5V8a4 4 0 0 1 8 0v2.5" />
  </Svg>
);
export const IconRefresh = (p: IconProps) => (
  <Svg {...p}>
    <path d="M20 11a8 8 0 0 0-14.3-4.9L4 8M4 4v4h4" />
    <path d="M4 13a8 8 0 0 0 14.3 4.9L20 16M20 20v-4h-4" />
  </Svg>
);
export const IconMail = (p: IconProps) => (
  <Svg {...p}>
    <rect x="3" y="5.5" width="18" height="13" rx="2" />
    <path d="M3.5 7l8.5 6 8.5-6" />
  </Svg>
);

/** Step illustrations for the export guide (FR-49). Generic, no Snap branding (NFR-LEG-01). */
export function GuideArt(props: { step: 1 | 2 | 3 | 4 }) {
  const common = {
    width: 88,
    height: 72,
    viewBox: "0 0 88 72",
    fill: "none",
    "aria-hidden": "true" as const,
    focusable: "false" as const,
    class: "guide-art",
  };
  switch (props.step) {
    case 1:
      return (
        <svg {...common}>
          <rect x="6" y="8" width="76" height="56" rx="6" class="art-surface" />
          <path d="M6 20h76" class="art-line" />
          <circle cx="14" cy="14" r="2" class="art-fill" />
          <circle cx="21" cy="14" r="2" class="art-fill" />
          <rect x="30" y="11" width="44" height="6" rx="3" class="art-soft" />
          <circle cx="44" cy="36" r="8" class="art-line" />
          <path d="M32 56c2-7 7-10 12-10s10 3 12 10" class="art-line" />
        </svg>
      );
    case 2:
      return (
        <svg {...common}>
          <rect x="10" y="10" width="68" height="52" rx="6" class="art-surface" />
          <rect x="18" y="20" width="10" height="10" rx="2" class="art-accent" />
          <path d="M20.5 25l2.3 2.3 4-4.6" class="art-check" />
          <rect x="34" y="23" width="34" height="4" rx="2" class="art-soft" />
          <rect x="18" y="40" width="10" height="10" rx="2" class="art-accent" />
          <path d="M20.5 45l2.3 2.3 4-4.6" class="art-check" />
          <rect x="34" y="43" width="28" height="4" rx="2" class="art-soft" />
        </svg>
      );
    case 3:
      return (
        <svg {...common}>
          <rect x="14" y="12" width="60" height="50" rx="6" class="art-surface" />
          <path d="M14 26h60" class="art-line" />
          <path d="M28 6v12M60 6v12" class="art-line" />
          <rect x="24" y="34" width="40" height="18" rx="9" class="art-accent" />
          <path d="M33 43h22" class="art-check" />
        </svg>
      );
    case 4:
      return (
        <svg {...common}>
          <rect x="8" y="16" width="50" height="38" rx="5" class="art-surface" />
          <path d="M9 18l24 18 24-18" class="art-line" />
          <circle cx="64" cy="46" r="15" class="art-accent" />
          <path d="M64 38v11M59.5 45l4.5 4.5 4.5-4.5M58 54h12" class="art-check" />
        </svg>
      );
  }
}
