/** Which edges of a scroll area have more content hidden behind them. */
export type ScrollEdges = { top: boolean; bottom: boolean };

/** Sub-pixel slack: browsers report fractional scroll positions on zoomed or scaled displays. */
const SLACK = 1;

/** Pure: from the three scroll metrics of an element to the edges that should show a shadow. */
export function scrollEdges(scrollTop: number, clientHeight: number, scrollHeight: number): ScrollEdges {
  const overflow = scrollHeight - clientHeight;
  if (overflow <= SLACK) return { top: false, bottom: false };
  return { top: scrollTop > SLACK, bottom: scrollTop < overflow - SLACK };
}
