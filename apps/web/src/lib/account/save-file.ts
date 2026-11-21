/** Saves `text` as a downloaded file in the browser (nothing is kept afterwards). */
export function saveTextFile(text: string, fileName: string, type = "application/json"): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  try {
    followDownload(url, fileName);
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), 30_000);
  }
}

/** Starts a download from `url` (a one-time link is used once, straight away). */
export function followDownload(url: string, fileName?: string): void {
  const a = document.createElement("a");
  a.href = url;
  if (fileName) a.download = fileName;
  a.rel = "noopener noreferrer";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
}
