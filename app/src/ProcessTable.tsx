import type { ProcessList } from "./api";
import { bytes } from "./format";

const SHOWN = 15;

export function ProcessTable({ list }: { list: ProcessList }) {
  const rows = [...list.processes].sort((a, b) => b.working_set - a.working_set).slice(0, SHOWN);
  return (
    <section aria-labelledby="proc-h" className="card">
      <h2 id="proc-h">Largest processes</h2>
      <p className="lede">
        Top {rows.length} of {list.processes.length} by working set (the memory each currently holds in RAM).
      </p>
      <table className="plain procs">
        <thead>
          <tr>
            <th scope="col">Process</th>
            <th scope="col" className="num">PID</th>
            <th scope="col" className="num">Memory</th>
            <th scope="col" className="num">CPU</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((p) => (
            <tr key={p.pid}>
              <th scope="row">{p.name}</th>
              <td className="num dim">{p.pid}</td>
              <td className="num">{bytes(p.working_set)}</td>
              <td className="num">{p.cpu_percent.toFixed(1)}%</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
