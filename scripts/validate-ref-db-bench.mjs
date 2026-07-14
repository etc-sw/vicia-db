export const ENGINES = ["vicia", "grafeo", "sqlite", "redb", "fjall", "turso", "cozo"];
export const ENGINE_AGGREGATE = ["vicia", "grafeo", "sqlite", "turso", "cozo"];
export const OWNED_RESULT_SCAN = ["redb", "fjall"];

export function expectedShape(profile) {
  if (profile === "smoke") return { facts: 10_000, repetitions: 5, trials: 1 };
  if (profile === "full") return { facts: 1_000_000, repetitions: 20, trials: 5 };
  throw new Error(`unknown profile: ${profile}`);
}

export function validateReceipts(receipts, profile) {
  const shape = expectedShape(profile);
  const expectedChecksum = shape.facts * (shape.facts - 1) / 2;
  const expectedCount = ENGINES.length * shape.trials;
  if (!Array.isArray(receipts) || receipts.length !== expectedCount) {
    throw new Error(`expected ${expectedCount} receipts, got ${receipts?.length}`);
  }

  const identities = new Set();
  const trialSeeds = new Map();
  for (const receipt of receipts) {
    if (receipt.schema !== "vicia.ref-db-bench.v5") throw new Error(`${receipt.engine}: schema`);
    if (!ENGINES.includes(receipt.engine)) throw new Error(`${receipt.engine}: unknown engine`);
    if (receipt.facts !== shape.facts) throw new Error(`${receipt.engine}: facts`);
    if (receipt.repetitions !== shape.repetitions) throw new Error(`${receipt.engine}: repetitions`);
    if (!Number.isInteger(receipt.trial) || receipt.trial < 0 || receipt.trial >= shape.trials) {
      throw new Error(`${receipt.engine}: trial`);
    }
    const identity = `${receipt.engine}:${receipt.trial}`;
    if (identities.has(identity)) throw new Error(`${identity}: duplicate`);
    identities.add(identity);

    if (!Number.isInteger(receipt.seed) || receipt.seed <= 0) throw new Error(`${identity}: seed`);
    const priorSeed = trialSeeds.get(receipt.trial);
    if (priorSeed !== undefined && priorSeed !== receipt.seed) throw new Error(`${identity}: seed mismatch`);
    trialSeeds.set(receipt.trial, receipt.seed);

    const expectedPosition = (ENGINES.indexOf(receipt.engine) - receipt.trial + ENGINES.length) % ENGINES.length;
    if (receipt.orderPosition !== expectedPosition) throw new Error(`${identity}: order position`);
    if (receipt.executionBoundary !== boundaryFor(receipt.engine)) throw new Error(`${identity}: boundary`);
    if (!receipt.adapterSchema || !receipt.semanticScope) throw new Error(`${identity}: adapter metadata`);
    if (!receipt.durability?.barrier || !receipt.durability?.batch) throw new Error(`${identity}: durability`);
    if (receipt.engine === "sqlite") {
      if (receipt.durability.journalMode !== "delete" || receipt.durability.synchronous !== "full") {
        throw new Error("sqlite: durability pragmas");
      }
      if (!receipt.runtimeVersion) throw new Error("sqlite: runtime version");
    }
    if (receipt.reopenVerified !== true) throw new Error(`${identity}: reopen`);
    finiteNonnegative(receipt.build?.elapsedMs, `${identity}: build elapsed`);
    finiteNonnegative(receipt.build?.baselineRssBytes, `${identity}: build baseline RSS`);
    finiteNonnegative(receipt.build?.peakRssBytes, `${identity}: build peak RSS`);
    validateCounters(receipt.build?.processDelta, `${identity}: build counters`);

    const query = receipt.query;
    finiteNonnegative(query?.openMs, `${identity}: open`);
    finiteNonnegative(query?.firstReadMs, `${identity}: first read`);
    validatePoint(query?.pointHot, shape.repetitions, `${identity}: hot`);
    validatePoint(query?.pointDistributed, shape.repetitions, `${identity}: distributed`);
    validatePoint(query?.pointMiss, shape.repetitions, `${identity}: miss`);
    validateSamples(query?.aggregateSamplesMs, shape.repetitions, `${identity}: aggregate`);
    if (query?.count !== shape.facts || query?.checksum !== expectedChecksum) {
      throw new Error(`${identity}: correctness`);
    }
    for (const field of [
      "openBaselineRssBytes", "workloadPeakRssBytes", "workloadDeltaRssBytes", "retainedRssBytes",
    ]) finiteNonnegative(query?.[field], `${identity}: ${field}`);
    validateCounters(query?.processDelta, `${identity}: query counters`);
    finiteNonnegative(receipt.storageBytes, `${identity}: storage`);
  }

  for (let trial = 0; trial < shape.trials; trial++) {
    if (!trialSeeds.has(trial)) throw new Error(`trial ${trial}: missing`);
  }
  return shape;
}

function boundaryFor(engine) {
  return OWNED_RESULT_SCAN.includes(engine) ? "ownedResultScan" : "engineAggregate";
}

function validatePoint(point, repetitions, label) {
  if (!Number.isInteger(point?.operationsPerSample) || point.operationsPerSample <= 0) {
    throw new Error(`${label}: operations`);
  }
  validateSamples(point.samplesMsPerOperation, repetitions, label);
}

function validateSamples(samples, repetitions, label) {
  if (!Array.isArray(samples) || samples.length !== repetitions) throw new Error(`${label}: samples`);
  for (const sample of samples) finiteNonnegative(sample, label);
}

function validateCounters(counters, label) {
  for (const field of ["readBytes", "writeBytes", "minorFaults", "majorFaults"]) {
    finiteNonnegative(counters?.[field], `${label}: ${field}`);
  }
}

function finiteNonnegative(value, label) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`${label}: expected finite nonnegative number`);
  }
}
