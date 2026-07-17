#ifndef TORCHNEXUS_AGENT_STORAGE_H
#define TORCHNEXUS_AGENT_STORAGE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct torchnexus_agent_storage_recorder torchnexus_agent_storage_recorder;
typedef struct torchnexus_agent_storage_bundle torchnexus_agent_storage_bundle;
typedef struct torchnexus_agent_storage_closed_bundle torchnexus_agent_storage_closed_bundle;

enum {
    TORCHNEXUS_AGENT_STORAGE_DIRECTION_CLIENT_TO_SERVER = 0,
    TORCHNEXUS_AGENT_STORAGE_DIRECTION_SERVER_TO_CLIENT = 1
};

int torchnexus_agent_storage_recorder_new(
    const char *root_dir,
    bool save_uncaptured_sessions,
    bool flush_each_chunk,
    torchnexus_agent_storage_recorder **out_recorder
);

int torchnexus_agent_storage_recorder_start_bundle(
    torchnexus_agent_storage_recorder *recorder,
    bool capture_enabled,
    torchnexus_agent_storage_bundle **out_bundle
);

int torchnexus_agent_storage_bundle_write_chunk(
    torchnexus_agent_storage_bundle *bundle,
    uint8_t direction,
    const uint8_t *data,
    size_t len
);

int torchnexus_agent_storage_bundle_close(
    torchnexus_agent_storage_bundle *bundle,
    torchnexus_agent_storage_closed_bundle **out_closed
);

const char *torchnexus_agent_storage_closed_bundle_id(
    const torchnexus_agent_storage_closed_bundle *closed
);

const char *torchnexus_agent_storage_closed_bundle_path(
    const torchnexus_agent_storage_closed_bundle *closed
);

uint64_t torchnexus_agent_storage_closed_bundle_file_size(
    const torchnexus_agent_storage_closed_bundle *closed
);

uint64_t torchnexus_agent_storage_closed_bundle_record_count(
    const torchnexus_agent_storage_closed_bundle *closed
);

const char *torchnexus_agent_storage_last_error_message(void);

void torchnexus_agent_storage_recorder_free(torchnexus_agent_storage_recorder *recorder);
void torchnexus_agent_storage_bundle_free(torchnexus_agent_storage_bundle *bundle);
void torchnexus_agent_storage_closed_bundle_free(torchnexus_agent_storage_closed_bundle *closed);

#endif
