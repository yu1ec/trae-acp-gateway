import { LOG_LEVELS, type LogLevel } from "../../lib/logParse";

interface LogToolbarProps {
  search: string;
  onSearchChange: (value: string) => void;
  levels: Set<LogLevel>;
  onToggleLevel: (level: LogLevel) => void;
}

export default function LogToolbar({
  search,
  onSearchChange,
  levels,
  onToggleLevel,
}: LogToolbarProps) {
  return (
    <div className="log-toolbar">
      <input
        type="search"
        className="log-search"
        placeholder="Search logs…"
        value={search}
        onChange={(e) => onSearchChange(e.target.value)}
      />
      <div className="level-filters">
        {LOG_LEVELS.map((level) => (
          <button
            key={level}
            type="button"
            className={`level-btn lv-${level}${levels.has(level) ? " active" : ""}`}
            onClick={() => onToggleLevel(level)}
          >
            {level}
          </button>
        ))}
      </div>
    </div>
  );
}
