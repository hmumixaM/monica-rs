-- Original raw manifests can be external files rather than inline JSON.
CREATE TABLE crm.legacy_content_locations (
 content_object_id uuid PRIMARY KEY REFERENCES crm.content_objects(id),
 encoding text,storage_uri text,created_at timestamptz
);
