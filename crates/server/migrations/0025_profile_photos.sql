-- Profile photos: the image is in file storage (photos/<user id>); this says when it was set
-- (NULL: none), which also makes its address change with each new photo.
ALTER TABLE users ADD COLUMN photo_at timestamptz;
