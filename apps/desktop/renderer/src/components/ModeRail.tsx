import { NavLink } from "react-router-dom";
import { Icon, type IconName } from "../icons/Icon";

const MODES: { to: string; label: string; icon: IconName }[] = [
  { to: "/", label: "Home", icon: "home" },
  { to: "/design", label: "Design", icon: "design" },
  { to: "/draft", label: "Draft", icon: "draft" },
  { to: "/bim", label: "BIM", icon: "bim" },
  { to: "/render", label: "Render", icon: "render" },
  { to: "/deliver", label: "Deliver", icon: "deliver" },
  { to: "/settings", label: "Settings", icon: "settings" },
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
            <Icon name={m.icon} size={22} />
          </span>
          <span>{m.label}</span>
        </NavLink>
      ))}
    </nav>
  );
}

export const MODE_RAIL_ITEMS = MODES;
