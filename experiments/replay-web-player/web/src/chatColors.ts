export function chatChannelColor(channel: string): string {
  if (channel === 'team') return '#4fe0a0';
  if (channel === 'division') return '#ffd369';
  if (channel === 'system') return '#7b9189';
  return '#eef4f1';
}
