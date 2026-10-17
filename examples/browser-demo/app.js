let revision = 1;
document.querySelector("#apply").addEventListener("click", () => {
  const text = document.querySelector("#edit").value.trim().slice(0, 1000);
  if (!text) return;
  document.querySelector("#priority").textContent = text;
  document.querySelector("#revision").textContent = `Revision ${++revision}`;
});
