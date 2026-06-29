export async function openReview(folder) {
  window.pp.show('review');
  document.getElementById('review-title').textContent = folder;
  // Grid/detail/keyboard/export are implemented in Task 6.
}
