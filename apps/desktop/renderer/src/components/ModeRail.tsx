import { NavLink } from "react-router-dom";

const MODES = [
  { to: "/", label: "Home", icon: "⌂" },
  { to: "/design", label: "Design", icon: "✎" },
  { to: "/draft", label: "Draft", icon: "▱" },
  { to: "/bim", label: "BIM", icon: "◫" },
  { to: "/render", label: "Render", icon: "◉" },
  { to: "/deliver", label: "Deliver", icon: "⤓" },
  { to: "/settings", label: "Settings", icon: "⚙" },
];

export function ModeRail() {
  return (
    <nav className="mode-rail" aria-label="Workflow modes">
      <div className="mode-rail__brand" aria-hidden>
        Æ
      </div>
      {MODES.map((m) => (
        <NavLink
          key={m.to}
          to={m.to}
          end={m.to === "/"}
          className={({ isActive }) =>
            `mode-rail__item${isActive ? " is-active" : ""}`
          }
        >
          <span className="mode-rail__icon" aria-hidden>
            {m.icon}
          </span>
          <span>{m.label}</span>
        </NavLink>
      ))}
    </nav>
  );
}

export const MODE_RAIL_ITEMS = MODES;
