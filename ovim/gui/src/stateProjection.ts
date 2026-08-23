export const shouldAcceptRevision = (
    latestRevision: number | undefined,
    nextRevision: number,
) => latestRevision === undefined || nextRevision >= latestRevision;

export const retainProjection = <T>(previous: T, next: T): T => {
    if (Object.is(previous, next)) return previous;
    if (
        previous === null ||
        next === null ||
        typeof previous !== "object" ||
        typeof next !== "object"
    )
        return next;

    if (Array.isArray(previous) || Array.isArray(next)) {
        if (!Array.isArray(previous) || !Array.isArray(next)) return next;
        let unchanged = previous.length === next.length;
        const retained = next.map((value, index) => {
            const result = retainProjection(previous[index], value);
            if (!Object.is(result, previous[index])) unchanged = false;
            return result;
        });
        return (unchanged ? previous : retained) as T;
    }

    const prior = previous as Record<string, unknown>;
    const incoming = next as Record<string, unknown>;
    const priorKeys = Object.keys(prior);
    const incomingKeys = Object.keys(incoming);
    let unchanged = priorKeys.length === incomingKeys.length;
    const retained: Record<string, unknown> = {};
    for (const key of incomingKeys) {
        const result = retainProjection(prior[key], incoming[key]);
        retained[key] = result;
        if (!Object.is(result, prior[key])) unchanged = false;
    }
    return (unchanged ? previous : retained) as T;
};
