-- Run on an offline backup first, then on the stopped daemon's database.
-- Keep incident IDs, tariff history and measured energy unchanged.
BEGIN IMMEDIATE;
UPDATE samples SET device = 'usb-0db0-' || substr(device, 5),
    json = json_set(json, '$.device_id', 'usb-0db0-' || substr(device, 5))
    WHERE device GLOB 'msi-*';
UPDATE energy SET device = 'usb-0db0-' || substr(device, 5)
    WHERE device GLOB 'msi-*';
UPDATE incidents SET device = 'usb-0db0-' || substr(device, 5),
    json = json_set(json, '$.device_id', 'usb-0db0-' || substr(device, 5))
    WHERE device GLOB 'msi-*';
UPDATE incidents SET json = json_set(json, '$.trigger_sample.device_id',
    'usb-0db0-' || substr(json_extract(json, '$.trigger_sample.device_id'), 5))
    WHERE json_extract(json, '$.trigger_sample.device_id') GLOB 'msi-*';
UPDATE evidence SET json = json_set(json, '$.device_id',
    'usb-0db0-' || substr(json_extract(json, '$.device_id'), 5))
    WHERE json_extract(json, '$.device_id') GLOB 'msi-*';
COMMIT;
