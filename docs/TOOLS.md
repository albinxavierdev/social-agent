# Tool contracts

_Generated from `docs/openapi.json` (itself generated from the tool registry). Regenerate with `cargo run -q --bin content-agent-api -- --print-openapi > docs/openapi.json`._

Call any tool with `POST /internal/v1/tools/{tool}` and body `{"context": {...}, "input": {...}}`. The response body is the tool's contract JSON; `x-trace-id` and `x-request-id` headers are always returned. Errors use the envelope in [README](../README.md#errors).

| Tool | Needs permission action | Zernio connection | Zernio access | Purpose |
|---|---|---|---|---|
| `cancel_job` | draft | no | none | Cancel a job and its queued work |
| `extract_content` | read | no | none | Extract clean structured content from a source |
| `find_images` | draft | no | none | Find, validate, rank and store image candidates for a draft |
| `generate_content_report` | draft | no | none | Generate a structured content report from extracted sources |
| `generate_social_draft` | draft | no | none | Generate channel-specific drafts from a report |
| `get_draft` | read | no | none | Get a draft, its review events and its audit trail |
| `get_job` | read | no | none | Get a job, its sources and its audit trail |
| `ingest_url` | read | no | none | Create an ingestion job from one or more URLs |
| `prepare_publish_request` | per input | yes | write | Create a Zernio-ready publish/schedule request for an approved draft |
| `publish_draft` | publish | yes | write | Submit a prepared request to Zernio for immediate publishing |
| `read_comments` | read | yes | read | Read comments via Zernio |
| `read_inbox` | read | yes | read | Read inbox conversations via Zernio |
| `read_performance` | read | yes | read | Read performance analytics via Zernio |
| `read_x_post` | read | optional | read | Read a public X post |
| `read_youtube_video` | read | optional | read | Read a YouTube video's metadata |
| `repurpose_content` | draft | no | none | Repurpose a source, report or draft into multiple variants |
| `review_draft` | draft | no | none | Create or update the human-review state of a draft |
| `schedule_draft` | schedule | yes | write | Submit a prepared request to Zernio for scheduled publishing |
| `submit_review_decision` | draft | no | none | Record the customer's review decision |
| `validate_content` | draft | no | none | Run pre-publication checks on a draft |

## Input / output schemas

### `cancel_job`

Cancel a job and its queued work.

**input**
```json
{
  "properties": {
    "job_id": {
      "type": "string"
    },
    "reason": {
      "type": "string"
    }
  },
  "required": [
    "job_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `extract_content`

Extract clean structured content from a source.

**input**
```json
{
  "properties": {
    "force": {
      "type": "boolean"
    },
    "source_id": {
      "type": "string"
    },
    "want_screenshot": {
      "type": "boolean"
    }
  },
  "required": [
    "source_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "author": {
      "type": [
        "string",
        "null"
      ]
    },
    "content_type": {
      "type": [
        "string",
        "null"
      ]
    },
    "extraction_method": {
      "type": [
        "string",
        "null"
      ]
    },
    "extraction_status": {
      "type": "string"
    },
    "language": {
      "type": [
        "string",
        "null"
      ]
    },
    "metadata": {
      "type": "object"
    },
    "published_at": {
      "format": "date-time",
      "type": [
        "string",
        "null"
      ]
    },
    "screenshot_url": {
      "type": [
        "string",
        "null"
      ]
    },
    "source_id": {
      "type": "string"
    },
    "summary": {
      "type": [
        "string",
        "null"
      ]
    },
    "text": {
      "type": [
        "string",
        "null"
      ]
    },
    "title": {
      "type": [
        "string",
        "null"
      ]
    },
    "url": {
      "type": "string"
    }
  },
  "required": [
    "source_id",
    "extraction_status"
  ],
  "type": "object"
}
```

### `find_images`

Find, validate, rank and store image candidates for a draft.

**input**
```json
{
  "properties": {
    "allow_source_images": {
      "type": "boolean"
    },
    "count": {
      "maximum": 4,
      "minimum": 1,
      "type": "integer"
    },
    "draft_id": {
      "type": "string"
    }
  },
  "required": [
    "draft_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "draft_id": {
      "type": "string"
    },
    "images": {
      "items": {
        "properties": {
          "alt_text": {
            "type": "string"
          },
          "aspect_ratio": {
            "type": "string"
          },
          "height": {
            "type": "integer"
          },
          "image_id": {
            "type": "string"
          },
          "source": {
            "type": "string"
          },
          "url": {
            "type": "string"
          },
          "validation_status": {
            "type": "string"
          },
          "width": {
            "type": "integer"
          }
        },
        "required": [
          "image_id",
          "url"
        ],
        "type": "object"
      },
      "type": "array"
    }
  },
  "required": [
    "draft_id",
    "images"
  ],
  "type": "object"
}
```

### `generate_content_report`

Generate a structured content report from extracted sources.

**input**
```json
{
  "properties": {
    "brand_context": {
      "$ref": "#/components/schemas/BrandContext"
    },
    "campaign_goal": {
      "type": "string"
    },
    "requested_channels": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "source_ids": {
      "items": {
        "type": "string"
      },
      "type": "array"
    }
  },
  "required": [
    "source_ids"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "audience_takeaways": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "confidence": {
      "type": "number"
    },
    "content_pillars": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "key_points": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "recommended_angle": {
      "type": "string"
    },
    "recommended_channels": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "report_id": {
      "type": "string"
    },
    "risks": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "source_ids": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "source_links": {
      "items": {
        "type": "string"
      },
      "type": "array"
    }
  },
  "required": [
    "report_id"
  ],
  "type": "object"
}
```

### `generate_social_draft`

Generate channel-specific drafts from a report.

**input**
```json
{
  "properties": {
    "brand_context": {
      "$ref": "#/components/schemas/BrandContext"
    },
    "channels": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "include_cta": {
      "type": "boolean"
    },
    "include_hashtags": {
      "type": "boolean"
    },
    "post_type": {
      "type": "string"
    },
    "report_id": {
      "type": "string"
    }
  },
  "required": [
    "report_id",
    "channels"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "compliance": {
      "properties": {
        "status": {
          "type": "string"
        },
        "warnings": {
          "items": {
            "type": "string"
          },
          "type": "array"
        }
      },
      "required": [
        "status"
      ],
      "type": "object"
    },
    "draft_id": {
      "type": "string"
    },
    "report_id": {
      "type": "string"
    },
    "source_links": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "status": {
      "type": "string"
    },
    "variants": {
      "items": {
        "$ref": "#/components/schemas/Variant"
      },
      "type": "array"
    }
  },
  "required": [
    "draft_id",
    "status",
    "variants"
  ],
  "type": "object"
}
```

### `get_draft`

Get a draft, its review events and its audit trail.

**input**
```json
{
  "properties": {
    "draft_id": {
      "type": "string"
    }
  },
  "required": [
    "draft_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `get_job`

Get a job, its sources and its audit trail.

**input**
```json
{
  "properties": {
    "job_id": {
      "type": "string"
    }
  },
  "required": [
    "job_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `ingest_url`

Create an ingestion job from one or more URLs.

**input**
```json
{
  "properties": {
    "allow_duplicates": {
      "type": "boolean"
    },
    "brand_context": {
      "$ref": "#/components/schemas/BrandContext"
    },
    "content_type": {
      "type": "string"
    },
    "requested_channels": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "urls": {
      "items": {
        "type": "string"
      },
      "maxItems": 20,
      "minItems": 1,
      "type": "array"
    },
    "want_screenshot": {
      "type": "boolean"
    }
  },
  "required": [
    "urls"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "job_id": {
      "type": "string"
    },
    "sources": {
      "items": {
        "properties": {
          "extraction_strategy": {
            "type": "string"
          },
          "source_id": {
            "type": "string"
          },
          "source_type": {
            "type": "string"
          },
          "url": {
            "type": "string"
          }
        },
        "required": [
          "source_id"
        ],
        "type": "object"
      },
      "type": "array"
    },
    "status": {
      "type": "string"
    }
  },
  "required": [
    "job_id",
    "status",
    "sources"
  ],
  "type": "object"
}
```

### `prepare_publish_request`

Create a Zernio-ready publish/schedule request for an approved draft.

**input**
```json
{
  "properties": {
    "action": {
      "enum": [
        "publish",
        "schedule"
      ]
    },
    "channels": {
      "items": {
        "type": "string"
      },
      "type": "array"
    },
    "draft_id": {
      "type": "string"
    },
    "scheduled_at": {
      "format": "date-time",
      "type": "string"
    },
    "zernio_profile_id": {
      "type": "string"
    }
  },
  "required": [
    "draft_id",
    "zernio_profile_id",
    "channels",
    "action"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "action": {
      "type": "string"
    },
    "payload": {
      "properties": {
        "channels": {
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "content": {
          "type": "object"
        },
        "media": {
          "type": "array"
        },
        "zernio_request": {
          "type": "object"
        }
      },
      "required": [],
      "type": "object"
    },
    "publish_request_id": {
      "type": "string"
    },
    "scheduled_at": {
      "format": "date-time",
      "type": [
        "string",
        "null"
      ]
    },
    "status": {
      "type": "string"
    },
    "zernio_profile_id": {
      "type": "string"
    }
  },
  "required": [
    "publish_request_id",
    "status"
  ],
  "type": "object"
}
```

### `publish_draft`

Submit a prepared request to Zernio for immediate publishing.

**input**
```json
{
  "properties": {
    "async": {
      "type": "boolean"
    },
    "publish_request_id": {
      "type": "string"
    }
  },
  "required": [
    "publish_request_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `read_comments`

Read comments via Zernio.

**input**
```json
{
  "properties": {
    "cursor": {
      "type": "string"
    },
    "limit": {
      "type": "integer"
    },
    "platform": {
      "type": "string"
    },
    "zernio_profile_id": {
      "type": "string"
    }
  },
  "required": [
    "zernio_profile_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `read_inbox`

Read inbox conversations via Zernio.

**input**
```json
{
  "properties": {
    "cursor": {
      "type": "string"
    },
    "limit": {
      "type": "integer"
    },
    "platform": {
      "type": "string"
    },
    "zernio_profile_id": {
      "type": "string"
    }
  },
  "required": [
    "zernio_profile_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `read_performance`

Read performance analytics via Zernio.

**input**
```json
{
  "properties": {
    "cursor": {
      "type": "string"
    },
    "limit": {
      "type": "integer"
    },
    "platform": {
      "type": "string"
    },
    "zernio_profile_id": {
      "type": "string"
    }
  },
  "required": [
    "zernio_profile_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `read_x_post`

Read a public X post.

**input**
```json
{
  "properties": {
    "url": {
      "type": "string"
    }
  },
  "required": [
    "url"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `read_youtube_video`

Read a YouTube video's metadata.

**input**
```json
{
  "properties": {
    "url": {
      "type": "string"
    }
  },
  "required": [
    "url"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `repurpose_content`

Repurpose a source, report or draft into multiple variants.

**input**
```json
{
  "properties": {
    "brand_context": {
      "$ref": "#/components/schemas/BrandContext"
    },
    "draft_id": {
      "type": "string"
    },
    "report_id": {
      "type": "string"
    },
    "source_id": {
      "type": "string"
    },
    "targets": {
      "items": {
        "properties": {
          "channel": {
            "type": "string"
          },
          "format": {
            "type": "string"
          }
        },
        "required": [
          "channel",
          "format"
        ],
        "type": "object"
      },
      "type": "array"
    }
  },
  "required": [
    "targets"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "campaign_id": {
      "type": "string"
    },
    "variants": {
      "items": {
        "properties": {
          "channel": {
            "type": "string"
          },
          "draft_id": {
            "type": "string"
          },
          "format": {
            "type": "string"
          }
        },
        "required": [
          "draft_id"
        ],
        "type": "object"
      },
      "type": "array"
    }
  },
  "required": [
    "campaign_id",
    "variants"
  ],
  "type": "object"
}
```

### `review_draft`

Create or update the human-review state of a draft.

**input**
```json
{
  "properties": {
    "action": {
      "enum": [
        "submit",
        "status"
      ]
    },
    "draft_id": {
      "type": "string"
    }
  },
  "required": [
    "draft_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "draft_id": {
      "type": "string"
    },
    "job_id": {
      "type": "string"
    },
    "next_action": {
      "type": "string"
    },
    "review_url": {
      "type": "string"
    },
    "status": {
      "type": "string"
    }
  },
  "required": [
    "draft_id",
    "status"
  ],
  "type": "object"
}
```

### `schedule_draft`

Submit a prepared request to Zernio for scheduled publishing.

**input**
```json
{
  "properties": {
    "async": {
      "type": "boolean"
    },
    "publish_request_id": {
      "type": "string"
    }
  },
  "required": [
    "publish_request_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "type": "object"
}
```

### `submit_review_decision`

Record the customer's review decision.

**input**
```json
{
  "properties": {
    "decision": {
      "enum": [
        "approved",
        "rejected",
        "revision_requested",
        "cancelled"
      ]
    },
    "draft_id": {
      "type": "string"
    },
    "edited_content": {
      "type": "object"
    },
    "notes": {
      "type": "string"
    },
    "reviewer_id": {
      "type": "string"
    }
  },
  "required": [
    "draft_id",
    "decision",
    "reviewer_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "draft_id": {
      "type": "string"
    },
    "next_action": {
      "type": "string"
    },
    "review_event_id": {
      "type": "string"
    },
    "status": {
      "type": "string"
    }
  },
  "required": [
    "draft_id",
    "status"
  ],
  "type": "object"
}
```

### `validate_content`

Run pre-publication checks on a draft.

**input**
```json
{
  "properties": {
    "check_brand_rules": {
      "type": "boolean"
    },
    "draft_id": {
      "type": "string"
    }
  },
  "required": [
    "draft_id"
  ],
  "type": "object"
}
```
**output**
```json
{
  "properties": {
    "draft_id": {
      "type": "string"
    },
    "issues": {
      "items": {
        "$ref": "#/components/schemas/Issue"
      },
      "type": "array"
    },
    "status": {
      "enum": [
        "passed",
        "failed"
      ]
    },
    "warnings": {
      "items": {
        "type": "string"
      },
      "type": "array"
    }
  },
  "required": [
    "draft_id",
    "status"
  ],
  "type": "object"
}
```
