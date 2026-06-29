import { api, show } from '/app.js';
export async function startAnalyze(folder) {
  show('analyze');
  document.getElementById('view-analyze').innerHTML = `<div class="browse"><p>Starting analysis of ${folder}…</p></div>`;
  await api('POST', '/api/analyze', { folder });
  // Progress polling + transition to review are implemented in Task 6.
}
