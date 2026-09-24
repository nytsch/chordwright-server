# Kein npm install, kein apk: Server, Start und Zertifikat kommen mit Node allein
# aus. Der Build braucht nur das Basis-Image.
FROM node:22-alpine
WORKDIR /srv
COPY server/ ./server/
COPY start.mjs certs.mjs ./
CMD ["node", "start.mjs"]
