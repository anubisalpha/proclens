import type { MemoryStats, ProcessList } from "./api";
import { bytes, percent } from "./format";

interface Segment {
  key: string;
  label: string;
  value: number;
  hint: string;
}

/** Splits physical RAM into non-overlapping buckets. Needs the page lists (elevated collector). */
function segmentsFor(m: MemoryStats): Segment[] {
  const l = m.lists;
  if (!l) {
    return [
      { key: "inuse", label: "In use or cached", value: m.physical_total - m.physical_available, hint: "Not split further without administrator access." },
      { key: "free", label: "Available", value: m.physical_available, hint: "Free plus cache Windows can reclaim instantly." },
    ];
  }
  const segs: Segment[] = [
    { key: "inuse", label: "In use", value: l.in_use, hint: "Process memory, drivers and the kernel. Derived as total minus the buckets below." },
    { key: "modified", label: "Modified", value: l.modified, hint: "Changed data waiting to be written to disk before it can be reused." },
    { key: "standby", label: "Standby (cache)", value: l.standby, hint: "Cached file data. Windows hands this to programs the moment they need it." },
    { key: "free", label: "Free", value: l.free_zero, hint: "Completely unused." },
  ];
  if (l.bad > 0) segs.push({ key: "bad", label: "Bad", value: l.bad, hint: "Pages the system has marked faulty." });
  return segs;
}

export function MemoryView({ memory, processes }: { memory: MemoryStats; processes: ProcessList | null }) {
  const segs = segmentsFor(memory);
  const total = memory.physical_total;
  const commitPct = percent(memory.commit_total, memory.commit_limit);

  return (
    <section aria-labelledby="mem-h" className="card">
      <h2 id="mem-h">Memory</h2>
      <p className="lede">
        {bytes(total)} installed. {bytes(memory.physical_available)} is available to programs right now.
      </p>

      <div className="bar" role="img" aria-label={`Physical memory split: ${segs.map((s) => `${s.label} ${bytes(s.value)}`).join(", ")}`}>
        {segs.map((s) => (
          <div key={s.key} className={`seg seg-${s.key}`} style={{ flexGrow: s.value }} title={`${s.label}: ${bytes(s.value)}`} />
        ))}
      </div>

      <table className="legend">
        <tbody>
          {segs.map((s) => (
            <tr key={s.key}>
              <td><span className={`swatch seg-${s.key}`} aria-hidden="true" /></td>
              <th scope="row">{s.label}</th>
              <td className="num">{bytes(s.value)}</td>
              <td className="num dim">{percent(s.value, total)}</td>
              <td className="dim hint">{s.hint}</td>
            </tr>
          ))}
        </tbody>
      </table>

      {!memory.lists && (
        <p className="note">
          The breakdown into in-use, cached, modified and free needs the collector running as administrator.
        </p>
      )}

      <h3>Where the "in use" memory sits</h3>
      <table className="plain">
        <tbody>
          <tr>
            <th scope="row">Kernel nonpaged pool</th>
            <td className="num">{bytes(memory.kernel_nonpaged)}</td>
            <td className="dim hint">Memory drivers and Windows can never page out.</td>
          </tr>
          <tr>
            <th scope="row">Kernel paged pool (virtual)</th>
            <td className="num">{bytes(memory.kernel_paged)}</td>
            <td className="dim hint">Size of the pool; only part of it is resident in RAM.</td>
          </tr>
          <tr>
            <th scope="row">Process working sets, summed</th>
            <td className="num">{processes ? bytes(processes.total_working_set) : "…"}</td>
            <td className="dim hint">
              Counts memory shared between processes once for each one, so this can exceed "in use".
            </td>
          </tr>
          <tr>
            <th scope="row">Commit charge</th>
            <td className="num">{bytes(memory.commit_total)} of {bytes(memory.commit_limit)}</td>
            <td className="dim hint">{commitPct} of the limit (RAM plus page file) promised to programs.</td>
          </tr>
        </tbody>
      </table>

      <p className="counts dim">
        {memory.process_count} processes · {memory.thread_count.toLocaleString()} threads · {memory.handle_count.toLocaleString()} handles
      </p>
    </section>
  );
}
