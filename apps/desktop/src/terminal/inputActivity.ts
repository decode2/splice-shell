export function shouldRefreshTargetAfterInput(data: string) {
  return (
    data.includes("\r") ||
    data.includes("\n") ||
    data.includes("\x03") ||
    data.includes("\x1b")
  );
}
