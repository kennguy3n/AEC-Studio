import { Routes, Route, Navigate } from "react-router-dom";
import { ModeRail } from "./components/ModeRail";
import { StatusBar } from "./components/StatusBar";
import { Home } from "./pages/Home";
import { Design } from "./pages/Design";
import { Draft } from "./pages/Draft";
import { Bim } from "./pages/Bim";
import { Render } from "./pages/Render";
import { Deliver } from "./pages/Deliver";

export default function App() {
  return (
    <div className="app-shell">
      <ModeRail />
      <main className="app-main">
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/design" element={<Design />} />
          <Route path="/draft" element={<Draft />} />
          <Route path="/bim" element={<Bim />} />
          <Route path="/render" element={<Render />} />
          <Route path="/deliver" element={<Deliver />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </main>
      <StatusBar />
    </div>
  );
}
