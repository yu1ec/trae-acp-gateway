import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import LogsPage from "./pages/LogsPage";
import SettingsPage from "./pages/SettingsPage";

export default function App() {
  return (
    <HashRouter>
      <Routes>
        <Route path="/settings" element={<SettingsPage />} />
        <Route path="/logs" element={<LogsPage />} />
        <Route path="*" element={<Navigate to="/settings" replace />} />
      </Routes>
    </HashRouter>
  );
}
