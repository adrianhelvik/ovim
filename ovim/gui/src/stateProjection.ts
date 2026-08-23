export const shouldAcceptRevision = (
    latestRevision: number | undefined,
    nextRevision: number,
) => latestRevision === undefined || nextRevision >= latestRevision;
