export function createLatestRequest() {
  let activeController;
  return async (request) => {
    activeController?.abort();
    const controller = new AbortController();
    activeController = controller;
    try {
      const result = await request(controller.signal);
      return controller === activeController ? result : undefined;
    } finally {
      if (controller === activeController) activeController = undefined;
    }
  };
}

export function tenantFreshnessTelemetry(outcome) {
  if (!['fresh', 'stale', 'failed'].includes(outcome)) throw new TypeError('invalid freshness outcome');
  return { event: 'tenant_configuration_freshness', outcome };
}
