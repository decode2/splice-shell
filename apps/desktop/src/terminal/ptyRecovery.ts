export type ClosedPtyRecoveryState = {
  currentGeneration: number;
  failedGeneration: number;
  inputClosed: boolean;
};

export function shouldRecoverClosedPtyInput({
  currentGeneration,
  failedGeneration,
  inputClosed,
}: ClosedPtyRecoveryState) {
  return !inputClosed && failedGeneration === currentGeneration;
}
