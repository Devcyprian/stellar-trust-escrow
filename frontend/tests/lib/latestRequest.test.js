import { createLatestRequest, tenantFreshnessTelemetry } from '../../lib/latestRequest';

test('aborts stale payment-token availability requests', async () => {
  const latest = createLatestRequest();
  let firstSignal;
  const first = latest((signal) => { firstSignal = signal; return new Promise(() => {}); });
  await latest(() => Promise.resolve('new'));
  expect(firstSignal.aborted).toBe(true);
  void first;
});

test('freshness telemetry excludes tenant identifiers', () => {
  expect(tenantFreshnessTelemetry('stale')).toEqual({ event: 'tenant_configuration_freshness', outcome: 'stale' });
  expect(() => tenantFreshnessTelemetry('tenant-id')).toThrow();
});
